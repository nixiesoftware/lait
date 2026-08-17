#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "session counters and request lengths are constrained by World limits"
)]
//! [`Session`] — a local caller docked to a hosted World.
//!
//! A Session is bound to one World, principal, and Station activation epoch.
//! Sessions are many-to-one, independently closeable, and **cannot** stop the
//! Station. Authorization is checked per request, not only at Dock.
//!
//! The dispatch seam: `submit`/`query` **validate the request against the
//! World's registration, contain a World panic, and build a bounded**
//! [`Context`](crate::world::Context) over the principal before routing
//! to the World implementation. Before the World is called the Session
//! enforces: the Station is live; the payload is within
//! [`Limits`](crate::world::Limits); the intent/query names a declared
//! schema+version (a query may also read a declared readable predecessor); and
//! the principal's standing is **re-resolved through the mechanics
//! [`AuthorityView`](crate::world::AuthorityView)** for this request. A panic in
//! the callback is caught as [`Failure::CallbackPanicked`] and never
//! ends the Station.
//!
//! After the World stages its effect, the Session **contains** it — every staged
//! operation and scope must address the Session's own World namespace with an
//! operation kind that World's registered mutation models allow — then performs
//! the authority-frontier compare-and-swap under the writer lock, and durably
//! commits. Success means recoverable, not merely applied in memory.

use crate::poison::LockRecovering;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use mechanics::{authorization::AuthorizationDemand, station::Epoch};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::body::{MutationModel, Op, Schema};
use replica::frontier::ReplicaFrontier;
use serde::{Deserialize, Serialize};

use crate::world::{
    AuthorityView, Context, DeniedCause, Effect, Intent, Limits, PrincipalFacts, Projection, Query,
    Rejection, World,
};

/// A concurrency or idempotency conflict observed while a Session commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    AuthorityChanged,
    Request,
    Body,
}

/// A Session operation that could not produce a semantic result.
///
/// World-owned decisions remain visibly nested under `Rejected`; host
/// interruption, concurrency, callback containment, and durability are not
/// allowed to masquerade as World decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Rejected(Rejection),
    Conflict(Conflict),
    Interrupted,
    Persistence,
    /// Durable derived state failed with a concrete operation and cause.
    PersistenceCause {
        operation: &'static str,
        reason: String,
    },
    Reset,
    CallbackPanicked,
    /// The requested World generation is well-formed but this Station does not
    /// retain its material. Distinct from reset/interruption: callers may fall
    /// back to a nearer ancestor or request the generation from another holder.
    GenerationUnavailable,
    /// Authority state could not be evaluated at all — the ledger failed to
    /// materialize the pinned frontier (missing history, malformed frontier,
    /// or a durable failure). A local-state problem, never a standing denial:
    /// rendering it as "denied" once sent a fully-granted member to ask an
    /// admin for a grant they already held.
    AuthorityUnavailable(String),
}

/// A peer-neutral coordinate for one World's read generation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorldSnapshotId {
    pub world: WorldId,
    pub root: [u8; 32],
}

impl WorldSnapshotId {
    pub fn new(world: WorldId, root: [u8; 32]) -> Self {
        Self { world, root }
    }

    pub fn to_hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.root)
    }
}

/// One retained point in a World's generation lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldGeneration {
    pub id: WorldSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<WorldSnapshotId>,
    pub frontier: ReplicaFrontier,
}

impl From<Rejection> for Failure {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rejected(rejection) => Some(rejection),
            _ => None,
        }
    }
}

/// A resumable Observation position. First observation, restart, cursor overrun,
/// schema migration, or lost continuity forces a reset/rebaseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCursor {
    pub epoch: Epoch,
    pub sequence: u64,
}

impl ObservationCursor {
    /// The starting cursor — its first delivery always resets.
    pub fn start(epoch: Epoch) -> Self {
        Self { epoch, sequence: 0 }
    }
}

/// A bounded invalidation/advancement signal published after a durable commit.
/// It carries no replicated state — consumers re-query. A slow consumer
/// rebaselines rather than buffering without bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub epoch: Epoch,
    pub sequence: u64,
    /// Set on first observation, restart, cursor overrun, migration, or lost
    /// continuity — the consumer must rebaseline.
    pub reset: bool,
    /// Every Body this change touched, across Worlds.
    ///
    /// A `BodyKey` names its own World, so grouping is recoverable from this
    /// alone — a separate `world` field could only ever disagree with it, and
    /// one durable change that spans Worlds is still one change.
    pub bodies: Vec<BodyKey>,
    /// The Space's **authority** advanced in this same change (membership,
    /// roles, devices, keys).
    ///
    /// A plane of its own because authority is not a Body: it converges through
    /// signed authority records, so it can move with no scope at all and without
    /// the Body frontier changing. A record may carry this alone.
    pub authority: bool,
    pub frontier: ReplicaFrontier,
    /// Exact immutable read coordinates affected by this durable change,
    /// sorted by World. Scalar text offsets in `change` apply only when the
    /// viewer's projection carries the matching coordinate; otherwise it must
    /// re-query (anchors remain available for server-side continuity).
    pub publications: Vec<AffectedWorldPublication>,
    /// Authenticated, bounded feedback from the same durable change. It never
    /// carries Body values; consumers read `frontier`/the current publication
    /// for state. Older/remote changes may be coarse `Body dirty` records.
    pub change: crate::change::DurableChange,
}

/// One affected World and the exact Station-local publication installed (or
/// made explicitly unavailable) at this Observation's durable frontier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AffectedWorldPublication {
    pub world: WorldId,
    pub publication: crate::publication::WorldPublicationId,
}

/// The result of a durable [`Session::submit`]: the application-defined effect
/// bytes, the **committed** Replica frontier the change advanced to, and the
/// Observation Bodies it touched. A `CommittedEffect` is proof of durability —
/// it is returned only after the Replica advanced from a real Engine receipt.
/// An identical replay of the same request returns the identical
/// `CommittedEffect` without reapplying anything; invalidation delivery is the
/// job of [`Session::observe`], not of this return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedEffect {
    pub effect: Vec<u8>,
    pub frontier: ReplicaFrontier,
    pub bodies: Vec<BodyKey>,
    /// The exact publication prepared before durability and installed with the
    /// acknowledgement. Idempotent replay reads these semantic coordinates
    /// from the durable receipt rather than mutable current package state.
    pub publication: crate::publication::WorldPublicationId,
}

/// The single mutex-guarded committing state: the Replica writer plus the
/// closed flag. `closed` lives **inside** the same mutex as the writer so that
/// commit admission and Station shutdown are one serialized state transition —
/// a submit admitted before dormancy either commits before the close (and is
/// durable + checkpointed) or observes `closed` and is refused. There is no
/// window where a commit lands after the shutdown checkpoint.
struct CoreInner {
    replica: replica::Replica,
    snapshot: Arc<replica::ReadSnapshot>,
    snapshot_materialization: crate::publication::MaterializationId,
    next_materialization: crate::publication::MaterializationId,
    generations: std::collections::BTreeMap<[u8; 32], CachedReadGeneration>,
    parents: std::collections::BTreeMap<[u8; 32], Option<[u8; 32]>>,
    generation_order: std::collections::VecDeque<[u8; 32]>,
    world_publications: std::collections::BTreeMap<WorldId, Arc<WorldPublication>>,
    retained_world_publications: std::collections::BTreeMap<
        (WorldId, crate::publication::WorldPublicationId),
        Arc<WorldPublication>,
    >,
    world_publication_order:
        std::collections::VecDeque<(WorldId, crate::publication::WorldPublicationId)>,
    cursor_leases:
        std::collections::BTreeMap<(WorldId, crate::publication::WorldPublicationId), CursorLease>,
    publication_flights: std::collections::BTreeMap<
        (WorldId, crate::publication::PublicationId),
        Arc<PublicationFlight>,
    >,
    world_builders: std::collections::BTreeMap<WorldId, WorldPublicationBuilder>,
    /// Signed Offer news for this activation. Lossy: not a reserved Body.
    offers: std::collections::BTreeMap<crate::exec::OfferId, crate::exec::Offer>,
    /// Outstanding nonce-bound readiness challenges.
    challenges:
        std::collections::BTreeMap<(crate::exec::OfferId, [u8; 16]), crate::exec::Challenge>,
    /// Accepted Ready answers, one live window per Offer.
    readies: std::collections::BTreeMap<crate::exec::OfferId, AcceptedReady>,
    closed: bool,
}

struct AcceptedReady {
    challenge: crate::exec::Challenge,
    ready: crate::exec::Ready,
}

struct OfferAdmission<'a> {
    news: &'a std::collections::BTreeMap<crate::exec::OfferId, crate::exec::Offer>,
    readies: &'a std::collections::BTreeMap<crate::exec::OfferId, AcceptedReady>,
    now_millis: u64,
}

#[derive(Debug)]
struct WorldPublication {
    id: crate::publication::WorldPublicationId,
    snapshot: Arc<replica::ReadSnapshot>,
    corpus: Arc<crate::corpus::Corpus>,
}

struct CursorLease {
    publication: Arc<WorldPublication>,
    expires: std::time::Instant,
    estimated_bytes: u64,
}

struct PublicationFlight {
    result: std::sync::Mutex<Option<Result<Arc<WorldPublication>, crate::find::Failure>>>,
    wake: std::sync::Condvar,
}

impl PublicationFlight {
    fn new() -> Self {
        Self {
            result: std::sync::Mutex::new(None),
            wake: std::sync::Condvar::new(),
        }
    }

    fn complete(&self, result: Result<Arc<WorldPublication>, crate::find::Failure>) {
        *self.result.lock_recovering() = Some(result);
        self.wake.notify_all();
    }

    fn wait(&self) -> Result<Arc<WorldPublication>, crate::find::Failure> {
        let mut result = self.result.lock_recovering();
        loop {
            if let Some(result) = result.clone() {
                return result;
            }
            result = self.wake.wait(result).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Callback-local capability over one already-selected immutable publication.
/// It owns no mutable state and no authority evaluator; `gates` is the exact
/// result Runtime derived for this principal before entering World code.
struct ContextFindReader {
    publication: Arc<WorldPublication>,
    schemas: Arc<[crate::find::Schema]>,
    policy: crate::find::Policy,
    gates: crate::find_evaluator::GrantedGates,
    epoch: Epoch,
    space: mechanics::ids::SpaceId,
    world: WorldId,
    implementation: [u8; 32],
    actor: mechanics::ids::ActorId,
    device: mechanics::ids::DeviceId,
    authority_frontier: replica::frontier::AuthorityFrontier,
    issued_cursor: Arc<std::sync::atomic::AtomicBool>,
}

impl crate::world::FindReader for ContextFindReader {
    fn publication(&self) -> crate::publication::WorldPublicationId {
        self.publication.id
    }

    fn find(&self, query: crate::find::Query) -> Result<crate::find::Answer, crate::find::Failure> {
        if query
            .publication
            .is_some_and(|requested| requested != self.publication.id.publication)
        {
            return Err(crate::find::Failure::PublicationUnavailable);
        }
        let declaration = self
            .schemas
            .iter()
            .find(|schema| schema.reference == query.schema)
            .ok_or(crate::find::Invalid::UndeclaredSchema("query schema"))?;
        query.validate_within_schema(declaration)?;
        if !self.policy.bound.contains(query.bound) {
            return Err(crate::find::Failure::PolicyExceeded);
        }
        let query_digest = query.digest()?;
        let coordinates = crate::find::Coordinates {
            epoch: self.epoch,
            space: self.space.clone(),
            world: self.world.clone(),
            implementation: self.implementation,
            root: self.publication.id.publication.manifest_root,
            extractor_schema_digest: self.publication.id.publication.extractor_schema_digest,
            materialization: self.publication.id.materialization,
            actor: self.actor.clone(),
            device: self.device.clone(),
            authority_frontier: self.authority_frontier.clone(),
            query: query_digest,
            schema: query.schema.clone(),
        };
        let answer = crate::find::evaluate(crate::find::Admission {
            query,
            coordinates,
            policy: self.policy,
            snapshot: self.publication.snapshot.clone(),
            corpus: self.publication.corpus.clone(),
            gates: self.gates.clone(),
        })?;
        if answer.next_cursor().is_some() {
            self.issued_cursor
                .store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(answer)
    }
}

#[derive(Clone)]
struct WorldPublicationBuilder {
    world: Arc<dyn World>,
    implementation: [u8; 32],
    extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
    schemas: Vec<crate::find::Schema>,
    extractors: Vec<crate::find::Extractor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationFailure {
    Interrupted,
    Extractor,
    Corpus,
}

#[derive(Clone)]
struct CachedReadGeneration {
    snapshot: Arc<replica::ReadSnapshot>,
    materialization: crate::publication::MaterializationId,
}

/// Hot immutable generations retained by one active Station. Durable lineage
/// is complete in Replica; this only bounds reconstructed snapshots in RAM.
const CACHED_READ_GENERATIONS: usize = 64;
/// Shared hot corpora across current and historical generations. Durable
/// generations remain reconstructable after eviction; this only bounds RAM and
/// makes expiry explicit to local cursors.
pub(crate) const CACHED_WORLD_PUBLICATIONS: usize = 256;
/// Active cursor continuations are leases, not hints. Count bounds retained
/// publication memory globally per Station; a full table refuses a new cursor
/// rather than evicting one another request can still present.
const MAX_CURSOR_LEASES: usize = 256;
const CURSOR_LEASE_TTL: std::time::Duration = std::time::Duration::from_secs(120);
/// Conservative physical-retention ceiling for all distinct cursor-pinned
/// publications. The release fixture pairs a million-node Corpus with a
/// million source Bodies; exact Body-image bytes and calibrated compact-index
/// prices admit one such publication but deliberately refuse two. There is no
/// smaller per-publication ceiling: paging is specifically how supported large
/// corpora are consumed.
///
/// A lease of the current/hot publication adds only an `Arc`, not another
/// Corpus. The reservation accounts for the future point where that unique Arc
/// would otherwise leave the hot cache. The map has one entry per exact World
/// publication, so concurrent cursors never double-charge the same allocation.
const MAX_CURSOR_LEASE_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn cursor_publication_estimate(snapshot_bytes: u64, corpus_bytes: u64) -> u64 {
    // Both component estimates are maintained incrementally at publication
    // time. Cursor admission stays O(1) even for a million record Bodies and
    // prices exact snapshot export sizes rather than a logical-row proxy.
    snapshot_bytes
        .saturating_add(corpus_bytes)
        .saturating_add(4096)
}

#[cfg(test)]
mod cursor_lease_budget_tests {
    use super::{cursor_publication_estimate, MAX_CURSOR_LEASE_RETAINED_BYTES};

    #[test]
    fn one_record_addressed_million_node_publication_fits_but_two_pins_do_not() {
        // One real collaborative export is 296 bytes in the scale fixture;
        // Snapshot pricing adds 400 fixed bytes + a four-byte stamp, while the
        // compact Corpus prices 34 logical + 288 node + 64 Body bytes.
        let estimate = cursor_publication_estimate(700 * 1_000_000, 386 * 1_000_000);
        assert!(estimate >= 953 * 1_024 * 1_024);
        assert!(estimate <= MAX_CURSOR_LEASE_RETAINED_BYTES);
        assert!(estimate.saturating_mul(2) > MAX_CURSOR_LEASE_RETAINED_BYTES);
    }
}

impl CoreInner {
    fn reserve_materialization(&mut self) -> crate::publication::MaterializationId {
        let materialization = self.next_materialization;
        self.next_materialization = materialization.next();
        materialization
    }

    fn cache_generation(
        &mut self,
        root: [u8; 32],
        snapshot: Arc<replica::ReadSnapshot>,
        parent: Option<[u8; 32]>,
    ) -> crate::publication::MaterializationId {
        let materialization = self.reserve_materialization();
        self.cache_generation_at(root, snapshot, parent, materialization);
        materialization
    }

    fn cache_generation_at(
        &mut self,
        root: [u8; 32],
        snapshot: Arc<replica::ReadSnapshot>,
        parent: Option<[u8; 32]>,
        materialization: crate::publication::MaterializationId,
    ) {
        self.generation_order.retain(|candidate| candidate != &root);
        self.generation_order.push_back(root);
        self.generations.insert(
            root,
            CachedReadGeneration {
                snapshot,
                materialization,
            },
        );
        self.parents.insert(root, parent);
        while self.generations.len() > CACHED_READ_GENERATIONS {
            let Some(expired) = self.generation_order.pop_front() else {
                break;
            };
            if expired == self.snapshot.root() {
                self.generation_order.push_back(expired);
                continue;
            }
            self.generations.remove(&expired);
            self.parents.remove(&expired);
            self.retained_world_publications
                .retain(|(_, id), _| id.publication.manifest_root != expired);
            self.world_publication_order
                .retain(|(_, id)| id.publication.manifest_root != expired);
        }
    }

    fn purge_cursor_leases(&mut self) {
        let now = std::time::Instant::now();
        self.cursor_leases.retain(|_, lease| lease.expires > now);
        self.trim_world_publications();
    }

    fn leased_world_publication(
        &mut self,
        key: &(WorldId, crate::publication::WorldPublicationId),
    ) -> Option<Arc<WorldPublication>> {
        self.purge_cursor_leases();
        let lease = self.cursor_leases.get_mut(key)?;
        lease.expires = std::time::Instant::now() + CURSOR_LEASE_TTL;
        Some(lease.publication.clone())
    }

    fn lease_world_publication(
        &mut self,
        world: WorldId,
        publication: Arc<WorldPublication>,
    ) -> Result<(), ()> {
        self.purge_cursor_leases();
        let key = (world, publication.id);
        let estimate = cursor_publication_estimate(
            publication.snapshot.retained_bytes_estimate(),
            publication.corpus.retained_bytes_estimate(),
        );
        if let Some(lease) = self.cursor_leases.get_mut(&key) {
            lease.expires = std::time::Instant::now() + CURSOR_LEASE_TTL;
            lease.publication = publication;
            lease.estimated_bytes = estimate;
            return Ok(());
        }
        if self.cursor_leases.len() == MAX_CURSOR_LEASES {
            return Err(());
        }
        let retained = self.cursor_leases.values().fold(0u64, |total, lease| {
            total.saturating_add(lease.estimated_bytes)
        });
        if retained.saturating_add(estimate) > MAX_CURSOR_LEASE_RETAINED_BYTES {
            return Err(());
        }
        self.cursor_leases.insert(
            key,
            CursorLease {
                publication,
                expires: std::time::Instant::now() + CURSOR_LEASE_TTL,
                estimated_bytes: estimate,
            },
        );
        Ok(())
    }

    fn retain_world_publication(&mut self, world: WorldId, publication: Arc<WorldPublication>) {
        self.purge_cursor_leases();
        let key = (world, publication.id);
        self.world_publication_order
            .retain(|candidate| candidate != &key);
        self.world_publication_order.push_back(key.clone());
        self.retained_world_publications.insert(key, publication);

        self.trim_world_publications();
    }

    fn trim_world_publications(&mut self) {
        while self.retained_world_publications.len() > CACHED_WORLD_PUBLICATIONS {
            let attempts = self.world_publication_order.len();
            let mut removed = false;
            for _ in 0..attempts {
                let Some(candidate) = self.world_publication_order.pop_front() else {
                    break;
                };
                let current = self
                    .world_publications
                    .get(&candidate.0)
                    .is_some_and(|publication| publication.id == candidate.1);
                let leased = self.cursor_leases.contains_key(&candidate);
                if current || leased {
                    self.world_publication_order.push_back(candidate);
                } else {
                    self.retained_world_publications.remove(&candidate);
                    removed = true;
                    break;
                }
            }
            if !removed {
                break;
            }
        }
    }

    fn install_world_publication(&mut self, world: WorldId, publication: Arc<WorldPublication>) {
        self.world_publications
            .insert(world.clone(), publication.clone());
        self.retain_world_publication(world, publication);
    }

    fn receipt_publication(
        &mut self,
        receipt: &replica::receipt::RequestReceipt,
    ) -> Result<crate::publication::WorldPublicationId, Failure> {
        let semantic = crate::publication::PublicationId::new(
            receipt.manifest_root,
            receipt.implementation_digest,
            crate::publication::ExtractorSchemaDigest::from_digest(receipt.extractor_schema_digest),
        );
        if let Some(publication) = self
            .world_publications
            .get(&receipt.world)
            .filter(|publication| publication.id.publication == semantic)
            .or_else(|| {
                self.retained_world_publications
                    .iter()
                    .find(|((world, id), _)| world == &receipt.world && id.publication == semantic)
                    .map(|(_, publication)| publication)
            })
        {
            return Ok(publication.id);
        }
        let materialization = if receipt.manifest_root == self.snapshot.root() {
            self.snapshot_materialization
        } else if let Some(generation) = self.generations.get(&receipt.manifest_root) {
            generation.materialization
        } else {
            let snapshot = self
                .replica
                .read_generation(&receipt.manifest_root)
                .map_err(commit_failure)?
                .ok_or(Failure::PersistenceCause {
                    operation: "resolve idempotent receipt publication",
                    reason: "the durable receipt generation is unavailable".into(),
                })?;
            self.cache_generation(receipt.manifest_root, Arc::new(snapshot), None)
        };
        Ok(crate::publication::WorldPublicationId::new(
            semantic,
            materialization,
        ))
    }

    fn affected_publications(&self, bodies: &[BodyKey]) -> Vec<AffectedWorldPublication> {
        let worlds = bodies
            .iter()
            .map(|body| body.world.clone())
            .collect::<std::collections::BTreeSet<_>>();
        worlds
            .into_iter()
            .filter_map(|world| {
                let publication = self
                    .world_publications
                    .get(&world)
                    .map(|publication| publication.id)
                    .or_else(|| {
                        let builder = self.world_builders.get(&world)?;
                        Some(crate::publication::WorldPublicationId::new(
                            crate::publication::PublicationId::new(
                                self.snapshot.root(),
                                builder.implementation,
                                builder.extractor_schema_digest,
                            ),
                            self.snapshot_materialization,
                        ))
                    })?;
                Some(AffectedWorldPublication { world, publication })
            })
            .collect()
    }

    fn publish_snapshot(
        &mut self,
        snapshot: Arc<replica::ReadSnapshot>,
        parent: Option<[u8; 32]>,
        changed: Option<&[BodyKey]>,
    ) {
        let root = snapshot.root();
        let materialization = self.cache_generation(root, snapshot.clone(), parent);
        let prior = std::mem::take(&mut self.world_publications);
        if let Some(changed) = changed {
            for (world, publication) in prior {
                if changed.iter().any(|body| body.world == world) {
                    continue;
                }
                let semantic = publication.id.publication;
                let next = crate::publication::WorldPublicationId::new(
                    crate::publication::PublicationId::new(
                        root,
                        semantic.implementation_digest,
                        semantic.extractor_schema_digest,
                    ),
                    materialization,
                );
                if let Ok((corpus, _)) = publication.corpus.apply(crate::corpus::CorpusDelta {
                    base: publication.id,
                    next,
                    bodies: Vec::new(),
                }) {
                    self.install_world_publication(
                        world,
                        Arc::new(WorldPublication {
                            id: next,
                            snapshot: snapshot.clone(),
                            corpus: Arc::new(corpus),
                        }),
                    );
                }
            }
        }
        self.snapshot = snapshot;
        self.snapshot_materialization = materialization;
    }
}

/// The default Observation ring capacity, and its hard maximum.
pub const DEFAULT_OBSERVATION_CAPACITY: usize = 1024;
pub const MAX_OBSERVATION_CAPACITY: usize = 65_536;

/// Why an Observation stream ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interruption {
    /// The Station has gone dormant or exited; re-dock after reactivation.
    StationDormant,
}

impl std::fmt::Display for Interruption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Interruption {}

/// The Station-owned Observation broadcaster: a bounded ring of published
/// records plus the sequence source. Publication and cursor replay happen
/// under ONE lock, so a subscription can never fall between a commit and its
/// record.
pub(crate) struct Broadcaster {
    state: std::sync::Mutex<BroadcastState>,
    wake: std::sync::Condvar,
    epoch: Epoch,
}

struct BroadcastState {
    next_seq: u64,
    ring: std::collections::VecDeque<Observation>,
    capacity: usize,
    last_frontier: ReplicaFrontier,
    closed: bool,
}

impl Broadcaster {
    fn new(epoch: Epoch, capacity: usize, frontier: ReplicaFrontier) -> Self {
        Self {
            state: std::sync::Mutex::new(BroadcastState {
                next_seq: 1,
                ring: std::collections::VecDeque::new(),
                capacity: capacity.clamp(1, MAX_OBSERVATION_CAPACITY),
                last_frontier: frontier,
                closed: false,
            }),
            wake: std::sync::Condvar::new(),
            epoch,
        }
    }

    /// Publish ONE record for one durable change. Sequences are monotonic within
    /// the activation epoch; the ring discards its oldest record past capacity
    /// (slow consumers rebaseline, memory never grows unbounded).
    ///
    /// The unit is the semantic change, not the durability phase that produced
    /// it: a Contact that incorporates authority *and* Bodies is one record
    /// carrying both, because splitting it would force every consumer to handle
    /// a scopeless record just to learn something the next one repeats. Scopes
    /// may span Worlds; `authority` may stand alone.
    pub(crate) fn publish(
        &self,
        bodies: Vec<BodyKey>,
        frontier: ReplicaFrontier,
        authority: bool,
        publications: Vec<AffectedWorldPublication>,
    ) {
        self.publish_change(
            crate::change::DurableChange::dirty(bodies),
            frontier,
            authority,
            publications,
        );
    }

    /// Publish an already-bounded change description. The legacy Body list is
    /// derived from it here, so the invalidation and semantic feedback cannot
    /// disagree.
    pub(crate) fn publish_change(
        &self,
        change: crate::change::DurableChange,
        frontier: ReplicaFrontier,
        authority: bool,
        mut publications: Vec<AffectedWorldPublication>,
    ) {
        let mut state = self.state.lock_recovering();
        if state.closed {
            return;
        }
        let sequence = state.next_seq;
        state.next_seq += 1;
        state.last_frontier = frontier;
        let bodies = change
            .bodies
            .iter()
            .map(|change| change.body.clone())
            .collect();
        publications.sort();
        publications.dedup();
        let record = Observation {
            epoch: self.epoch,
            sequence,
            reset: false,
            bodies,
            authority,
            frontier,
            publications,
            change,
        };
        if state.ring.len() == state.capacity {
            state.ring.pop_front();
        }
        state.ring.push_back(record);
        self.wake.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock_recovering();
        state.closed = true;
        self.wake.notify_all();
    }
}

/// A bounded Observation stream: invalidation records, never state. First use,
/// a cursor from another epoch, or a ring overrun delivers exactly one reset
/// record (consumers re-query from the committed frontier); an in-window
/// cursor replays retained records and then follows live delivery. Dormancy
/// ends the stream with a typed [`Interruption::StationDormant`].
/// A stream is Station-wide, not World-scoped: it never filtered by World, and a
/// record's own `bodies` name theirs. Carrying a World here would only have been
/// able to imply a narrowing that does not happen.
pub struct ObservationStream {
    broadcaster: Arc<Broadcaster>,
    /// The last delivered sequence (exclusive replay position); `None` before
    /// the first delivery when no valid cursor was presented.
    position: Option<u64>,
}

impl ObservationStream {
    fn pull(&mut self, state: &BroadcastState) -> Option<Observation> {
        if let Some(position) = self.position {
            let oldest_retained = state.ring.front().map(|o| o.sequence);
            let newest = state.next_seq - 1;
            if position >= newest {
                return None; // caught up — wait for live delivery
            }
            // The next record we owe is position+1; if it is no longer
            // retained, that is an overrun: one reset, gap discarded.
            match oldest_retained {
                Some(oldest) if position + 1 >= oldest => {
                    state.ring.iter().find(|o| o.sequence > position).cloned()
                }
                _ => Some(self.reset_record(state)),
            }
        } else {
            Some(self.reset_record(state))
        }
    }

    fn reset_record(&self, state: &BroadcastState) -> Observation {
        Observation {
            epoch: self.broadcaster.epoch,
            sequence: state.next_seq - 1,
            reset: true,
            bodies: Vec::new(),
            // A reset says "trust nothing", which subsumes every plane; flagging
            // authority as well would only invite a consumer to treat the two as
            // separable when rebaselining.
            authority: false,
            frontier: state.last_frontier,
            publications: Vec::new(),
            change: crate::change::DurableChange::default(),
        }
    }

    /// The next record, waiting up to `timeout`. `Ok(None)` on timeout;
    /// [`Interruption::StationDormant`] once the Station closed.
    pub fn next_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<Observation>, Interruption> {
        let deadline = tokio::time::Instant::now() + timeout;
        let broadcaster = self.broadcaster.clone();
        let mut state = broadcaster.state.lock_recovering();
        loop {
            if state.closed {
                return Err(Interruption::StationDormant);
            }
            if let Some(record) = self.pull(&state) {
                self.position = Some(record.sequence);
                return Ok(Some(record));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let (next, timed_out) = broadcaster
                .wake
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|p| {
                    let inner = p.into_inner();
                    (inner.0, inner.1)
                });
            state = next;
            if timed_out.timed_out() {
                if state.closed {
                    return Err(Interruption::StationDormant);
                }
                if let Some(record) = self.pull(&state) {
                    self.position = Some(record.sequence);
                    return Ok(Some(record));
                }
                return Ok(None);
            }
        }
    }

    /// The next already-published record without waiting.
    pub fn try_next(&mut self) -> Result<Option<Observation>, Interruption> {
        self.next_timeout(std::time::Duration::ZERO)
    }
}

/// The Station's exclusive committing state, shared with its Sessions. Held
/// behind an `Arc` by the Station and every Session; a Session can commit
/// through it but never stop the Station.
pub struct StationCore {
    inner: std::sync::Mutex<CoreInner>,
    pub(crate) broadcaster: Arc<Broadcaster>,
    /// Bumped whenever Space authority advances.
    ///
    /// A bare counter, deliberately not a frontier: a watcher does not need to
    /// know *what* changed, only that its pinned view is stale and must be
    /// asked again. Carrying the frontier here would put an authority value on
    /// a channel that is not the authority, and give a reader something it
    /// could be tempted to act on without re-resolving.
    ///
    /// It is not the Observation ring. That ring's entries correspond to
    /// durable commits and are consumed by clients; this is a wake-up for the
    /// delivery planes, and a plane falling behind on it must not cost a
    /// client its cursor.
    authority_tick: tokio::sync::watch::Sender<u64>,
    exec: std::sync::Mutex<ExecGate>,
    exec_tick: tokio::sync::watch::Sender<u64>,
}

struct ExecGate {
    inflight: std::collections::BTreeSet<crate::exec::AttemptId>,
    busy: bool,
}

impl StationCore {
    /// A core wrapping a Replica directly, for tests that exercise a surface
    /// built over one without standing up a Station.
    #[doc(hidden)]
    pub fn for_test(replica: replica::Replica) -> Self {
        Self::new(Epoch::ZERO, DEFAULT_OBSERVATION_CAPACITY, replica)
    }

    pub(crate) fn new(
        epoch: Epoch,
        observation_capacity: usize,
        replica: replica::Replica,
    ) -> Self {
        let frontier = replica.frontier();
        let snapshot = Arc::new(replica.read_snapshot());
        let root = snapshot.root();
        let initial_materialization = crate::publication::MaterializationId::INITIAL;
        let generations = [(
            root,
            CachedReadGeneration {
                snapshot: snapshot.clone(),
                materialization: initial_materialization,
            },
        )]
        .into_iter()
        .collect();
        let parents = [(root, None)].into_iter().collect();
        Self {
            inner: std::sync::Mutex::new(CoreInner {
                replica,
                snapshot,
                snapshot_materialization: initial_materialization,
                next_materialization: initial_materialization.next(),
                generations,
                parents,
                generation_order: std::collections::VecDeque::from([root]),
                world_publications: std::collections::BTreeMap::new(),
                retained_world_publications: std::collections::BTreeMap::new(),
                world_publication_order: std::collections::VecDeque::new(),
                cursor_leases: std::collections::BTreeMap::new(),
                publication_flights: std::collections::BTreeMap::new(),
                world_builders: std::collections::BTreeMap::new(),
                offers: std::collections::BTreeMap::new(),
                challenges: std::collections::BTreeMap::new(),
                readies: std::collections::BTreeMap::new(),
                closed: false,
            }),
            broadcaster: Arc::new(Broadcaster::new(epoch, observation_capacity, frontier)),
            authority_tick: tokio::sync::watch::Sender::new(0),
            exec: std::sync::Mutex::new(ExecGate {
                inflight: std::collections::BTreeSet::new(),
                busy: false,
            }),
            exec_tick: tokio::sync::watch::Sender::new(0),
        }
    }

    pub fn exec_tick(&self) -> tokio::sync::watch::Receiver<u64> {
        self.exec_tick.subscribe()
    }

    pub(crate) fn note_exec(&self) {
        self.exec_tick.send_modify(|n| *n = n.saturating_add(1));
    }

    fn try_begin_perform(&self) -> bool {
        let mut gate = self.exec.lock_recovering();
        if gate.busy {
            return false;
        }
        gate.busy = true;
        true
    }

    fn end_perform(&self) {
        self.exec.lock_recovering().busy = false;
    }

    fn claim_attempt(&self, attempt: crate::exec::AttemptId) {
        self.exec.lock_recovering().inflight.insert(attempt);
    }

    fn release_attempt(&self, attempt: crate::exec::AttemptId) {
        self.exec.lock_recovering().inflight.remove(&attempt);
    }

    fn is_inflight(&self, attempt: crate::exec::AttemptId) -> bool {
        self.exec.lock_recovering().inflight.contains(&attempt)
    }

    /// Watch for authority advancing.
    ///
    /// A live session pins the authority view it was admitted at. Something has
    /// to tell it that view is stale, or a revoked peer keeps whatever it was
    /// holding until it happens to disconnect — which is not a bound.
    pub fn authority_tick(&self) -> tokio::sync::watch::Receiver<u64> {
        self.authority_tick.subscribe()
    }

    /// Announce that Space authority advanced. Called after the write is
    /// durable, never before.
    pub fn note_authority_advanced(&self) {
        // Authority/key arrival can change which retained Bodies are readable
        // without moving the Manifest. No old corpus remains addressable under
        // that same semantic root; the next exact package access rebuilds it
        // under a fresh local materialization.
        {
            let mut inner = self.lock();
            let snapshot = inner.snapshot.clone();
            let parent = inner.parents.get(&snapshot.root()).copied().flatten();
            inner.publish_snapshot(snapshot, parent, None);
        }
        self.authority_tick
            .send_modify(|n| *n = n.saturating_add(1));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CoreInner> {
        self.inner.lock_recovering()
    }

    pub(crate) fn frontier(&self) -> ReplicaFrontier {
        self.lock().replica.frontier()
    }

    pub(crate) fn affected_world_publications(
        &self,
        bodies: &[BodyKey],
    ) -> Vec<AffectedWorldPublication> {
        self.lock().affected_publications(bodies)
    }

    /// The two figures a storage read takes from the Replica: how many Bodies
    /// it holds, and when its material was last verified end to end.
    ///
    /// Both under one lock acquisition, and deliberately through neither a
    /// mutation nor metadata-publication seam. A storage observation must not
    /// mint a Runtime generation or touch a World corpus merely because it
    /// looked at two counters.
    ///
    /// Answered on a closed core too. `closed` forbids commits; it does not
    /// make the Bodies stop being there, and the count of what a dormant store
    /// holds is exactly the question a storage surface is asking.
    pub(crate) fn storage(&self) -> (u64, Option<u64>) {
        let inner = self.lock();
        (inner.replica.body_count(), inner.replica.verified_at_ms())
    }

    /// Read from the Replica under its exclusive ownership lock without
    /// publishing a new Runtime generation.
    ///
    /// **Exclusive is not a choice here, and it is worth knowing why before
    /// anyone tries to relax it.** The obvious improvement is a `RwLock`, so
    /// that read-only questions — resolving a caret anchor, reading a
    /// projection — stop queueing behind commits. It does not compile:
    /// `RwLock<T>: Sync` requires `T: Sync`, and `CoreInner` holds a Replica
    /// holding `dyn Engine + Send`, which is not `Sync` because the underlying
    /// collaborative document is not. Concurrent readers are not expressible
    /// at all, whatever the access pattern.
    ///
    /// So a caret resolve takes the same lock a commit takes. What bounds the
    /// damage is the rate: `gates::LIVE_DATAGRAMS` admits 64 items a second per
    /// connection and `slots::MAX_LIVE_SESSIONS` is 32, so the worst honest
    /// load is a few thousand microsecond-scale resolutions a second against a
    /// commit measured in milliseconds. That is a small duty cycle rather than
    /// a safe one, and it is the number to re-derive if either ceiling moves.
    ///
    /// The real fix, if it is ever needed, is a snapshot the reader owns — not
    /// a second kind of borrow on state that cannot be shared.
    pub fn with_replica_read<T>(
        &self,
        f: impl FnOnce(&replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        let inner = self.lock();
        if inner.closed {
            return Err(replica::transaction::commit::Failure::Illegitimate(
                "station dormant".into(),
            ));
        }
        f(&inner.replica)
    }

    /// Mutate Replica-local control state which changes neither the durable
    /// Manifest nor any readable Body (for example a pending content hold).
    pub fn with_replica_control<T>(
        &self,
        f: impl FnOnce(&mut replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        let mut inner = self.lock();
        if inner.closed {
            return Err(replica::transaction::commit::Failure::Illegitimate(
                "station dormant".into(),
            ));
        }
        f(&mut inner.replica)
    }

    /// Commit content-plane metadata. A changed signed root receives a new
    /// exact Runtime coordinate, while the complete Body image and every
    /// corpus branch are structurally shared.
    pub fn with_replica_metadata<T>(
        &self,
        f: impl FnOnce(&mut replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        let mut inner = self.lock();
        if inner.closed {
            return Err(replica::transaction::commit::Failure::Illegitimate(
                "station dormant".into(),
            ));
        }
        let before_root = inner.snapshot.root();
        let before_frontier = inner.snapshot.frontier();
        let result = f(&mut inner.replica);
        if result.is_ok()
            && (inner.replica.frontier() != before_frontier
                || Self::current_replica_root(&inner.replica) != before_root)
        {
            let snapshot = Arc::new(
                inner
                    .replica
                    .advance_read_snapshot_metadata(&inner.snapshot),
            );
            inner.publish_snapshot(snapshot, Some(before_root), Some(&[]));
        }
        result
    }

    /// Apply a mutation whose exact dirty Body set is already known. This is
    /// primarily a narrow test/maintenance seam; World user actions use the
    /// prepared publication path and Contact uses [`Self::with_replica_convergence`].
    pub fn with_replica_bodies<T>(
        &self,
        changed: &[BodyKey],
        f: impl FnOnce(&mut replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        let mut inner = self.lock();
        if inner.closed {
            return Err(replica::transaction::commit::Failure::Illegitimate(
                "station dormant".into(),
            ));
        }
        let before = inner.snapshot.root();
        let result = f(&mut inner.replica);
        if result.is_ok() && !changed.is_empty() {
            let snapshot = Arc::new(
                inner
                    .replica
                    .advance_read_snapshot(&inner.snapshot, changed),
            );
            let parent = (snapshot.root() != before)
                .then_some(before)
                .or_else(|| inner.parents.get(&before).copied().flatten());
            inner.publish_snapshot(snapshot, parent, Some(changed));
        }
        result
    }

    /// Incorporate one validated Contact and publish only the Bodies the
    /// convergence result says changed. The result is computed and installed
    /// while the Replica writer remains locked, so another commit cannot slip
    /// between remote durability and its immutable read image.
    pub fn with_replica_convergence(
        &self,
        f: impl FnOnce(
            &mut replica::Replica,
        ) -> Result<
            replica::convergence::ConvergenceOutcome,
            replica::transaction::commit::Failure,
        >,
    ) -> Result<replica::convergence::ConvergenceOutcome, replica::transaction::commit::Failure>
    {
        let mut inner = self.lock();
        if inner.closed {
            return Err(replica::transaction::commit::Failure::Illegitimate(
                "station dormant".into(),
            ));
        }
        let before = inner.snapshot.root();
        let result = f(&mut inner.replica);
        if let Ok(outcome) = &result {
            if !outcome.bodies.is_empty() {
                let affected_worlds: std::collections::BTreeSet<_> = outcome
                    .bodies
                    .iter()
                    .map(|body| body.world.clone())
                    .collect();
                let affected: Vec<_> = affected_worlds
                    .into_iter()
                    .filter_map(|world_id| {
                        let builder = inner.world_builders.get(&world_id)?.clone();
                        let prior = inner.world_publications.get(&world_id).cloned();
                        Some((world_id, builder, prior))
                    })
                    .collect();
                let snapshot = Arc::new(
                    inner
                        .replica
                        .advance_read_snapshot(&inner.snapshot, &outcome.bodies),
                );
                let parent = (snapshot.root() != before)
                    .then_some(before)
                    .or_else(|| inner.parents.get(&before).copied().flatten());
                inner.publish_snapshot(snapshot, parent, Some(&outcome.bodies));
                // Remote truth is already durable and cannot be refused for a
                // local extractor defect. Attempt only the affected registered
                // Worlds, using the same changed-Body delta path as a local
                // publication. A failure leaves this exact current coordinate
                // unavailable; it never restores or serves the prior corpus.
                for (world_id, builder, prior) in affected {
                    if let Err(failure) = advance_inner_world_publication(
                        &mut inner,
                        prior,
                        &builder.world,
                        &world_id,
                        builder.implementation,
                        builder.extractor_schema_digest,
                        &builder.schemas,
                        &builder.extractors,
                        &outcome.bodies,
                    ) {
                        tracing::error!(
                            ?failure,
                            world = %world_id,
                            "remote World publication is unavailable"
                        );
                    }
                }
            }
        }
        result
    }

    fn current_replica_root(replica: &replica::Replica) -> [u8; 32] {
        let manifest = replica.manifest_root();
        if manifest == replica::transaction::NO_PARENT_ROOT {
            replica.frontier().root
        } else {
            manifest
        }
    }

    /// Ensure one exact World package has a ready corpus over the current
    /// immutable read image before a Session is exposed.
    pub(crate) fn ensure_world_publication(
        &self,
        world: &Arc<dyn World>,
        world_id: &WorldId,
        implementation: [u8; 32],
        extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
        schemas: &[crate::find::Schema],
        extractors: &[crate::find::Extractor],
    ) -> Result<(), PublicationFailure> {
        let mut inner = self.lock();
        if inner.closed {
            return Err(PublicationFailure::Interrupted);
        }
        inner.world_builders.insert(
            world_id.clone(),
            WorldPublicationBuilder {
                world: world.clone(),
                implementation,
                extractor_schema_digest,
                schemas: schemas.to_vec(),
                extractors: extractors.to_vec(),
            },
        );
        ensure_inner_world_publication(
            &mut inner,
            world,
            world_id,
            implementation,
            extractor_schema_digest,
            schemas,
            extractors,
        )?;
        Ok(())
    }

    /// Close the core to further commits, as one transition under the writer
    /// mutex: an in-flight submit either completed its journaled durable commit
    /// before the close or observes it and is refused. Every acknowledged
    /// commit is already on disk, so closing needs no checkpoint. Observation
    /// streams end with a typed `StationDormant`.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.broadcaster.close();
    }
}

/// The per-submit authorizer: captures the mechanics [`AuthorityView`] and the
/// mutation's companion coordinates, and turns the built transaction-core
/// digest into a signed [`mechanics::authorization::AuthorizationReceipt`].
struct SessionAuthorizer<'a> {
    authority: &'a dyn AuthorityView,
    space: &'a mechanics::ids::SpaceId,
    world: &'a WorldId,
    actor: &'a mechanics::ids::ActorId,
    device: &'a mechanics::ids::DeviceId,
    authority_frontier: &'a replica::frontier::AuthorityFrontier,
    implementation_id: [u8; 32],
}

impl replica::transaction::TransactionAuthorizer for SessionAuthorizer<'_> {
    fn authorize(
        &self,
        core: &replica::transaction::Core,
    ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        self.authority.authorize_mutation(
            self.space,
            self.world,
            self.actor,
            self.device,
            self.authority_frontier,
            core.parent_manifest_root,
            self.implementation_id,
            core.intent_digest,
            &core.demand,
            core.operations_digest,
            core.digest(),
        )
    }
}

/// The Live plane's caret reads over the current immutable read image.
///
/// Implemented here rather than in `live.rs` because this is the type that owns
/// publication selection. Each call pins the shared snapshot under the Station
/// lock, releases it, then mints or resolves the anchor without holding the
/// committing writer.
impl crate::plane::live::AnchorSource for StationCore {
    fn anchor_in_body(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        let inner = self.lock();
        if inner.closed {
            return None;
        }
        let snapshot = inner.snapshot.clone();
        drop(inner);
        snapshot.anchor(key, path, position)
    }

    fn resolve_anchor(&self, key: &BodyKey, anchor: &fabric::Anchor) -> fabric::AnchorResolution {
        // Total, so a dormant core is `Drifted` rather than an error — the
        // renderer's contract is that this never fails and never lies, not that
        // it always knows.
        let inner = self.lock();
        if inner.closed {
            return fabric::AnchorResolution::Drifted;
        }
        let snapshot = inner.snapshot.clone();
        drop(inner);
        snapshot.resolve_anchor(key, anchor)
    }
}

/// A [`BodyReader`] over a locked Replica, handed to a World during a query.
struct ReplicaReader<'a> {
    replica: &'a replica::Replica,
    snapshot: &'a replica::ReadSnapshot,
}

fn world_readable(binding: Option<&replica::body::BodyBinding>) -> bool {
    binding.is_some_and(|binding| !crate::exec::is_reserved_schema(&binding.schema))
}

fn validate_operation_partition(
    world: &WorldId,
    effect: &Effect,
    runtime: &RuntimeEffect,
) -> Result<(), Rejection> {
    let operation_count = effect
        .operations
        .len()
        .checked_add(runtime.operations.len())
        .ok_or(Rejection::LimitExceeded)?;
    if operation_count > replica::transaction::MAX_OPS_PER_TRANSACTION {
        return Err(Rejection::LimitExceeded);
    }
    let world_keys = effect
        .operations
        .iter()
        .map(|(key, _)| key)
        .chain(
            effect
                .declarations
                .iter()
                .map(|declaration| &declaration.key),
        )
        .chain(effect.content_refs.iter().map(|(key, _)| key))
        .chain(effect.bodies.iter())
        .collect::<Vec<_>>();
    if runtime
        .bindings
        .iter()
        .any(|(runtime_key, _)| &runtime_key.world != world || world_keys.contains(&runtime_key))
        || runtime
            .bindings
            .iter()
            .enumerate()
            .any(|(index, (key, _))| {
                runtime
                    .bindings
                    .iter()
                    .skip(index + 1)
                    .any(|(other, _)| other == key)
            })
        || runtime
            .operations
            .iter()
            .any(|(key, _)| !runtime.bindings.iter().any(|(bound, _)| bound == key))
        || runtime
            .content_refs
            .iter()
            .any(|(key, _)| !runtime.bindings.iter().any(|(bound, _)| bound == key))
        || runtime
            .bodies
            .iter()
            .any(|key| !runtime.bindings.iter().any(|(bound, _)| bound == key))
    {
        return Err(Rejection::ContractViolation);
    }
    Ok(())
}

#[derive(Debug)]
enum PerformAction {
    Try(crate::exec::Try),
    Begin {
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
    },
    Invoke {
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
    },
    Recover {
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
    },
}

fn kind_digest(kind: &crate::exec::RunEventKind) -> [u8; 32] {
    let tag: &[u8] = match kind {
        crate::exec::RunEventKind::Began(_) => b"began",
        crate::exec::RunEventKind::Saved(_) => b"saved",
        crate::exec::RunEventKind::Returned(_) => b"returned",
        crate::exec::RunEventKind::Failed(_) => b"failed",
        _ => b"event",
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/exec/perform/1\0");
    hasher.update(tag);
    *hasher.finalize().as_bytes()
}

fn ambient_failure(failure: AmbientFailure) -> Failure {
    match failure {
        AmbientFailure::NoActiveImplementation => {
            Failure::Rejected(Rejection::NoActiveImplementation)
        }
        AmbientFailure::ImplementationUnavailable => {
            Failure::Rejected(Rejection::ImplementationUnavailable)
        }
        AmbientFailure::AuthorityUnavailable(detail) => Failure::AuthorityUnavailable(detail),
    }
}

#[derive(Default)]
struct RuntimeEffect {
    operations: Vec<(BodyKey, Op)>,
    bindings: Vec<(BodyKey, replica::body::BodyBinding)>,
    content_refs: Vec<(BodyKey, Vec<replica::content::ContentRef>)>,
    bodies: Vec<BodyKey>,
    demands: Vec<Vec<u8>>,
}

fn exec_invalid(invalid: crate::exec::Invalid) -> Rejection {
    match invalid {
        crate::exec::Invalid::TooLarge => Rejection::LimitExceeded,
        _ => Rejection::ContractViolation,
    }
}

struct LowerRun {
    run: crate::exec::Run,
    start: crate::exec::Start,
    event_count: u64,
    heads: Vec<crate::exec::EventId>,
    staged_attempts: u32,
    terminal_staged: bool,
}

fn load_lower_run(
    snapshot: &replica::ReadSnapshot,
    world: &WorldId,
    run: crate::exec::RunId,
) -> Result<LowerRun, Rejection> {
    let (run, start, event_count) = crate::exec::read_committed_run(snapshot, world, run)
        .map_err(exec_invalid)?
        .ok_or(Rejection::ContractViolation)?;
    let heads = run.heads.clone();
    Ok(LowerRun {
        run,
        start,
        event_count,
        heads,
        staged_attempts: 0,
        terminal_staged: false,
    })
}

fn run_binding() -> Result<replica::body::BodyBinding, Rejection> {
    Ok(replica::body::BodyBinding {
        schema: SchemaId::parse(crate::exec::RUN_BODY_SCHEMA)
            .ok_or(Rejection::ContractViolation)?,
        schema_version: crate::exec::RUN_BODY_SCHEMA_VERSION,
        encoding: EncodingId::parse(crate::exec::BODY_ENCODING)
            .ok_or(Rejection::ContractViolation)?,
        mutation_model: replica::body::MUTATION_COLLABORATIVE,
    })
}

fn build_binding() -> Result<replica::body::BodyBinding, Rejection> {
    Ok(replica::body::BodyBinding {
        schema: SchemaId::parse(crate::exec::BUILD_BODY_SCHEMA)
            .ok_or(Rejection::ContractViolation)?,
        schema_version: crate::exec::BUILD_BODY_SCHEMA_VERSION,
        encoding: EncodingId::parse(crate::exec::BODY_ENCODING)
            .ok_or(Rejection::ContractViolation)?,
        mutation_model: replica::body::MUTATION_COLLABORATIVE,
    })
}

fn retain_runtime_content(
    lowered: &mut RuntimeEffect,
    key: &BodyKey,
    content: &[replica::content::ContentRef],
) {
    if content.is_empty() {
        return;
    }
    if let Some((_, retained)) = lowered
        .content_refs
        .iter_mut()
        .find(|(candidate, _)| candidate == key)
    {
        retained.extend_from_slice(content);
        retained.sort_unstable();
        retained.dedup();
    } else {
        lowered.content_refs.push((key.clone(), content.to_vec()));
    }
}

fn append_run_event(
    lowered: &mut RuntimeEffect,
    state: &mut LowerRun,
    world: &WorldId,
    kind: crate::exec::RunEventKind,
    demand: Vec<u8>,
    content: &[replica::content::ContentRef],
) -> Result<(), Rejection> {
    let next_count = state
        .event_count
        .checked_add(1)
        .ok_or(Rejection::LimitExceeded)?;
    if next_count > u64::from(state.run.started.limits.events) {
        return Err(Rejection::LimitExceeded);
    }
    let event = crate::exec::RunEvent::new(state.heads.clone(), kind).map_err(exec_invalid)?;
    let event_id = event.id().map_err(exec_invalid)?;
    let value = event.encode().map_err(exec_invalid)?;
    let key = BodyKey {
        world: world.clone(),
        body: BodyId::from_bytes(state.run.id.as_bytes()),
    };
    lowered.operations.push((
        key.clone(),
        Op::ListInsert {
            path: crate::exec::RUN_EVENTS_PATH.to_string(),
            index: state.event_count,
            value,
        },
    ));
    if !lowered
        .bindings
        .iter()
        .any(|(candidate, _)| candidate == &key)
    {
        lowered.bindings.push((key.clone(), run_binding()?));
    }
    if !lowered.bodies.contains(&key) {
        lowered.bodies.push(key.clone());
    }
    retain_runtime_content(lowered, &key, content);
    lowered.demands.push(demand);
    state.event_count = next_count;
    state.heads = vec![event_id];
    Ok(())
}

fn returned_after_began(attempt: &crate::exec::Attempt) -> bool {
    let [began] = attempt.began.as_slice() else {
        return false;
    };
    if !began.predecessors.contains(&attempt.leased_event) {
        return false;
    }
    let mut checkpoints = attempt.checkpoints.iter().collect::<Vec<_>>();
    checkpoints.sort_by_key(|fact| fact.value.checkpoint.sequence);
    let mut predecessor = began.event;
    let mut expected_sequence = 1u32;
    for checkpoint in checkpoints {
        if checkpoint.value.checkpoint.sequence != expected_sequence
            || !checkpoint.predecessors.contains(&predecessor)
        {
            return false;
        }
        let Some(next) = expected_sequence.checked_add(1) else {
            return false;
        };
        expected_sequence = next;
        predecessor = checkpoint.event;
    }
    matches!(
        attempt.outcomes.as_slice(),
        [outcome] if outcome.predecessors.contains(&predecessor)
    )
}

fn prune_offer_news(inner: &mut CoreInner, now: u64) {
    inner.offers.retain(|_, held| held.usable_at(now).is_ok());
    inner
        .challenges
        .retain(|_, challenge| challenge.usable_at(now).is_ok());
    inner.readies.retain(|_, accepted| {
        accepted.challenge.usable_at(now).is_ok()
            && accepted.ready.validate_for(&accepted.challenge).is_ok()
    });
}

fn consume_first_use_readies(
    inner: &mut CoreInner,
    commands: &[crate::exec::Cmd],
    snapshot: &replica::ReadSnapshot,
    world: &WorldId,
) {
    for command in commands {
        let crate::exec::Cmd::Try(intent) = command else {
            continue;
        };
        let Some(offer) = &intent.offer else {
            continue;
        };
        let historical = crate::exec::read_committed_run(snapshot, world, intent.run)
            .ok()
            .flatten()
            .is_some_and(|(run, _, _)| {
                run.attempts
                    .iter()
                    .any(|attempt| attempt.offer == Some(offer.id))
            });
        if !historical {
            inner.readies.remove(&offer.id);
        }
    }
}

fn lower_exec(
    commands: &[crate::exec::Cmd],
    specs: &[crate::exec::Spec],
    ambient: &Ambient,
    request: [u8; 16],
    world_operations: usize,
    snapshot: &replica::ReadSnapshot,
    admission: &OfferAdmission<'_>,
) -> Result<RuntimeEffect, Rejection> {
    if world_operations > replica::transaction::MAX_OPS_PER_TRANSACTION {
        return Err(Rejection::LimitExceeded);
    }
    let mut lowered = RuntimeEffect::default();
    let mut runs = std::collections::BTreeMap::<crate::exec::RunId, LowerRun>::new();
    let mut first_use_offers = std::collections::BTreeSet::<crate::exec::OfferId>::new();
    for (ordinal, command) in commands.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| Rejection::LimitExceeded)?;
        match command {
            crate::exec::Cmd::Start(start) => {
                let spec = specs
                    .iter()
                    .find(|spec| spec.name == start.spec.name && spec.version == start.spec.version)
                    .ok_or(Rejection::ContractViolation)?;
                start.validate_with_spec(spec).map_err(exec_invalid)?;
                let command_bytes = command.encode().map_err(exec_invalid)?;
                let command_len =
                    u32::try_from(command_bytes.len()).map_err(|_| Rejection::LimitExceeded)?;
                let command_chunks = u32::try_from(
                    command_bytes
                        .len()
                        .div_ceil(crate::exec::MAX_RUN_COMMAND_CHUNK_BYTES),
                )
                .map_err(|_| Rejection::LimitExceeded)?;
                let run = crate::exec::derive_run_id(
                    &ambient.space,
                    &ambient.world,
                    &ambient.principal.device,
                    request,
                    ordinal,
                );
                let key = BodyKey {
                    world: ambient.world.clone(),
                    body: BodyId::from_bytes(run.as_bytes()),
                };
                if snapshot.binding(&key).is_some() {
                    return Err(Rejection::ContractViolation);
                }
                let started = crate::exec::Started {
                    space: ambient.space.clone(),
                    world: ambient.world.clone(),
                    run,
                    spec: start.spec.clone(),
                    world_implementation: ambient.implementation,
                    build: start.build,
                    invoker: ambient.principal.actor.clone(),
                    device: ambient.principal.device.clone(),
                    authority_frontier: ambient.principal.authority_frontier.clone(),
                    parent_manifest_root: ambient.root,
                    input: spec.input.schema.clone(),
                    input_digest: start.input_digest(spec).map_err(exec_invalid)?,
                    input_content: start.input.content.clone(),
                    input_content_bytes: start.input.content_bytes,
                    resources: start.resources.clone(),
                    limits: start.limits,
                    request,
                    command: ordinal,
                    parent: start.parent,
                    source: start.source,
                    service: start.service,
                    query_grants_digest: start.query_grants_digest().map_err(exec_invalid)?,
                    command_digest: command.digest().map_err(exec_invalid)?,
                    command_bytes: command_len,
                    command_chunks,
                };
                let event = crate::exec::RunEvent::started(started)
                    .and_then(|event| event.encode())
                    .map_err(exec_invalid)?;
                lowered.operations.push((key.clone(), Op::Create));
                lowered.operations.push((
                    key.clone(),
                    Op::ListInsert {
                        path: crate::exec::RUN_EVENTS_PATH.to_string(),
                        index: 0,
                        value: event,
                    },
                ));
                for (chunk, value) in command_bytes
                    .chunks(crate::exec::MAX_RUN_COMMAND_CHUNK_BYTES)
                    .enumerate()
                {
                    lowered.operations.push((
                        key.clone(),
                        Op::MapSet {
                            path: crate::exec::RUN_COMMAND_PATH.to_string(),
                            key: format!("{chunk:08x}"),
                            value: value.to_vec(),
                        },
                    ));
                }
                lowered.bindings.push((key.clone(), run_binding()?));
                retain_runtime_content(&mut lowered, &key, &start.input.content);
                lowered.bodies.push(key);
                lowered.demands.push(spec.access.start.clone());
            }
            crate::exec::Cmd::Try(intent) => {
                if let std::collections::btree_map::Entry::Vacant(entry) = runs.entry(intent.run) {
                    entry.insert(load_lower_run(snapshot, &ambient.world, intent.run)?);
                }
                let state = runs
                    .get_mut(&intent.run)
                    .ok_or(Rejection::ContractViolation)?;
                let spec = specs
                    .iter()
                    .find(|spec| {
                        spec.name == state.run.started.spec.name
                            && spec.version == state.run.started.spec.version
                    })
                    .ok_or(Rejection::ContractViolation)?;
                state.start.validate_with_spec(spec).map_err(exec_invalid)?;
                intent
                    .validate_with(state.run.started.limits)
                    .map_err(exec_invalid)?;
                let attempt_count = u32::try_from(state.run.attempts.len())
                    .map_err(|_| Rejection::LimitExceeded)?
                    .checked_add(state.staged_attempts)
                    .ok_or(Rejection::LimitExceeded)?;
                if !state.run.is_unresolved()
                    || state.terminal_staged
                    || intent.build != state.run.started.build
                    || attempt_count >= state.run.started.limits.attempts
                {
                    return Err(Rejection::ContractViolation);
                }
                if let Some(offer) = &intent.offer {
                    if offer.station != ambient.principal.station
                        || offer.station_epoch != ambient.epoch
                    {
                        return Err(Rejection::ContractViolation);
                    }
                    let historical = state
                        .run
                        .attempts
                        .iter()
                        .any(|attempt| attempt.offer == Some(offer.id));
                    match admission.news.get(&offer.id) {
                        Some(held) if held.usable_at(admission.now_millis).is_ok() => {
                            intent.validate_with_offer(held).map_err(exec_invalid)?;
                            if held.space != ambient.space
                                || held.world != ambient.world
                                || held.world_build != ambient.implementation
                            {
                                return Err(Rejection::ContractViolation);
                            }
                            if !historical {
                                if !first_use_offers.insert(offer.id) {
                                    return Err(Rejection::ContractViolation);
                                }
                                let ready = admission
                                    .readies
                                    .get(&offer.id)
                                    .ok_or(Rejection::ContractViolation)?;
                                ready
                                    .challenge
                                    .usable_at(admission.now_millis)
                                    .map_err(exec_invalid)?;
                                ready
                                    .ready
                                    .validate_for(&ready.challenge)
                                    .map_err(exec_invalid)?;
                                if ready.challenge.offer != offer.id
                                    || ready.challenge.station != ambient.principal.station
                                    || ready.challenge.station_epoch != ambient.epoch
                                    || ready.ready.signature.signer != ambient.principal.device
                                {
                                    return Err(Rejection::ContractViolation);
                                }
                            }
                            lowered.demands.push(spec.access.offer.clone());
                        }
                        Some(_) | None if historical => {
                            lowered.demands.push(spec.access.offer.clone());
                        }
                        _ => return Err(Rejection::ContractViolation),
                    }
                }
                if let Some(checkpoint) = &intent.checkpoint {
                    let saved = state.run.attempts.iter().any(|attempt| {
                        attempt.checkpoints.iter().any(|fact| {
                            fact.value.checkpoint.content == checkpoint.content
                                && fact.value.checkpoint.build == checkpoint.build
                                && fact.value.checkpoint.sequence == checkpoint.sequence
                        })
                    });
                    if !saved {
                        return Err(Rejection::ContractViolation);
                    }
                }
                let attempt = crate::exec::derive_attempt_id(
                    intent.run,
                    &ambient.principal.device,
                    request,
                    ordinal,
                );
                if state
                    .run
                    .attempts
                    .iter()
                    .any(|candidate| candidate.id == attempt)
                {
                    return Err(Rejection::ContractViolation);
                }
                let mut retained = Vec::new();
                if let Some(enforcement) = intent.enforcement {
                    if snapshot.content_descriptor(&enforcement).is_none() {
                        return Err(Rejection::ContractViolation);
                    }
                    retained.push(enforcement);
                }
                if let Some(checkpoint) = &intent.checkpoint {
                    retained.push(checkpoint.content);
                }
                retained.sort_unstable();
                retained.dedup();
                append_run_event(
                    &mut lowered,
                    state,
                    &ambient.world,
                    crate::exec::RunEventKind::Leased(crate::exec::Leased {
                        run: intent.run,
                        attempt,
                        station: ambient.principal.station.clone(),
                        station_epoch: ambient.epoch,
                        executor: ambient.principal.actor.clone(),
                        device: ambient.principal.device.clone(),
                        build: intent.build,
                        offer: intent.offer.as_ref().map(|offer| offer.id),
                        offer_epoch: intent.offer.as_ref().map(|offer| offer.epoch).unwrap_or(0),
                        resources: intent.resources.clone(),
                        enforcement: intent.enforcement,
                        limits: intent.limits,
                        lease: intent.lease.clone(),
                        checkpoint: intent.checkpoint.clone(),
                        fence: intent.fence,
                    }),
                    spec.access.control.clone(),
                    &retained,
                )?;
                state.staged_attempts = state
                    .staged_attempts
                    .checked_add(1)
                    .ok_or(Rejection::LimitExceeded)?;
            }
            crate::exec::Cmd::Cancel { run } => {
                if let std::collections::btree_map::Entry::Vacant(entry) = runs.entry(*run) {
                    entry.insert(load_lower_run(snapshot, &ambient.world, *run)?);
                }
                let state = runs.get_mut(run).ok_or(Rejection::ContractViolation)?;
                let spec = specs
                    .iter()
                    .find(|spec| {
                        spec.name == state.run.started.spec.name
                            && spec.version == state.run.started.spec.version
                    })
                    .ok_or(Rejection::ContractViolation)?;
                if !state.run.is_unresolved() || state.terminal_staged {
                    return Err(Rejection::ContractViolation);
                }
                append_run_event(
                    &mut lowered,
                    state,
                    &ambient.world,
                    crate::exec::RunEventKind::CancelAsked(crate::exec::CancelAsked {
                        run: *run,
                        actor: ambient.principal.actor.clone(),
                        device: ambient.principal.device.clone(),
                    }),
                    spec.access.control.clone(),
                    &[],
                )?;
            }
            crate::exec::Cmd::Accept { run, attempt }
            | crate::exec::Cmd::Reject { run, attempt } => {
                if let std::collections::btree_map::Entry::Vacant(entry) = runs.entry(*run) {
                    entry.insert(load_lower_run(snapshot, &ambient.world, *run)?);
                }
                let state = runs.get_mut(run).ok_or(Rejection::ContractViolation)?;
                let spec = specs
                    .iter()
                    .find(|spec| {
                        spec.name == state.run.started.spec.name
                            && spec.version == state.run.started.spec.version
                    })
                    .ok_or(Rejection::ContractViolation)?;
                let selected = state
                    .run
                    .attempts
                    .iter()
                    .find(|candidate| candidate.id == *attempt)
                    .ok_or(Rejection::ContractViolation)?;
                let [outcome] = selected.outcomes.as_slice() else {
                    return Err(Rejection::ContractViolation);
                };
                outcome.validate_with_spec(spec).map_err(exec_invalid)?;
                if !state.run.is_unresolved()
                    || state.terminal_staged
                    || selected.build != state.run.started.build
                    || !selected.failures.is_empty()
                    || !selected.cancellations.is_empty()
                    || !returned_after_began(selected)
                {
                    return Err(Rejection::ContractViolation);
                }
                let kind = if matches!(command, crate::exec::Cmd::Accept { .. }) {
                    crate::exec::RunEventKind::Accepted(crate::exec::Accepted {
                        run: *run,
                        attempt: *attempt,
                        actor: ambient.principal.actor.clone(),
                        device: ambient.principal.device.clone(),
                    })
                } else {
                    crate::exec::RunEventKind::Rejected(crate::exec::Rejected {
                        run: *run,
                        attempt: *attempt,
                        actor: ambient.principal.actor.clone(),
                        device: ambient.principal.device.clone(),
                    })
                };
                append_run_event(
                    &mut lowered,
                    state,
                    &ambient.world,
                    kind,
                    spec.access.accept.clone(),
                    &[],
                )?;
                state.terminal_staged = true;
            }
            crate::exec::Cmd::Retry { .. }
            | crate::exec::Cmd::Resume { .. }
            | crate::exec::Cmd::Drain { .. } => return Err(Rejection::ContractViolation),
        }
        if world_operations
            .checked_add(lowered.operations.len())
            .is_none_or(|count| count > replica::transaction::MAX_OPS_PER_TRANSACTION)
        {
            return Err(Rejection::LimitExceeded);
        }
    }
    Ok(lowered)
}

fn lower_lifecycle_event(
    snapshot: &replica::ReadSnapshot,
    specs: &[crate::exec::Spec],
    ambient: &Ambient,
    run: crate::exec::RunId,
    kind: crate::exec::RunEventKind,
    content: &[replica::content::ContentRef],
) -> Result<RuntimeEffect, Rejection> {
    let mut state = load_lower_run(snapshot, &ambient.world, run)?;
    let spec = specs
        .iter()
        .find(|spec| {
            spec.name == state.run.started.spec.name
                && spec.version == state.run.started.spec.version
        })
        .ok_or(Rejection::ContractViolation)?;
    let demand = match &kind {
        crate::exec::RunEventKind::Began(_)
        | crate::exec::RunEventKind::Saved(_)
        | crate::exec::RunEventKind::Returned(_)
        | crate::exec::RunEventKind::Failed(_) => spec.access.control.clone(),
        _ => return Err(Rejection::ContractViolation),
    };
    let mut lowered = RuntimeEffect::default();
    append_run_event(
        &mut lowered,
        &mut state,
        &ambient.world,
        kind,
        demand,
        content,
    )?;
    Ok(lowered)
}

fn work_reply(
    snapshot: &replica::ReadSnapshot,
    request: &crate::exec::WorkRequest,
) -> Result<crate::exec::WorkReply, crate::exec::WorkRefusal> {
    let state = crate::exec::work_state(snapshot, request.world(), request.run())?
        .ok_or_else(|| crate::exec::WorkRefusal::NotFound(request.run()))?;
    if matches!(
        request,
        crate::exec::WorkRequest::Watch { known_heads, .. } if known_heads == &state.heads
    ) {
        return Ok(crate::exec::WorkReply::Unchanged {
            world: state.world,
            run: state.run,
            heads: state.heads,
        });
    }
    Ok(crate::exec::WorkReply::State(state))
}

/// Derive a new physical Attempt from scheduling coordinates that are already
/// durable on the Run. Work callers select product intent (continue or resume),
/// never an Offer, fence, enforcement artifact, or Attempt limit.
///
/// A Started-only Run cannot cross this seam: the first Attempt is scheduling
/// truth and has not been committed yet. Reusing a prior Attempt is safe only
/// within the same Station activation and without a Service lease; either
/// otherwise needs a fresh scheduler decision rather than guessed coordinates.
fn continuation_try(
    snapshot: &replica::ReadSnapshot,
    specs: &[crate::exec::Spec],
    ambient: &Ambient,
    request: &crate::exec::WorkRequest,
) -> Result<crate::exec::Try, crate::exec::WorkRefusal> {
    let run_id = request.run();
    let (run, _, _) = crate::exec::read_committed_run(snapshot, request.world(), run_id)?
        .ok_or(crate::exec::WorkRefusal::NotFound(run_id))?;
    if !run.is_unresolved() {
        return Err(crate::exec::WorkRefusal::Unsupported(
            "this Run is already resolved",
        ));
    }
    if !run.cancel_asked.is_empty() {
        return Err(crate::exec::WorkRefusal::Unsupported(
            "this Run has a committed cancellation request and cannot be continued",
        ));
    }
    let spec = specs
        .iter()
        .find(|spec| spec.name == run.started.spec.name && spec.version == run.started.spec.version)
        .ok_or(crate::exec::WorkRefusal::Unsupported(
            "the Run's Spec is not available in this World implementation",
        ))?;
    let attempt_count = u32::try_from(run.attempts.len())
        .map_err(|_| crate::exec::WorkRefusal::Unsupported("the Run has too many Attempts"))?;
    if attempt_count >= run.started.limits.attempts {
        return Err(crate::exec::WorkRefusal::Unsupported(
            "this Run has exhausted its Attempt limit",
        ));
    }

    let terminal = |attempt: &&crate::exec::Attempt| {
        !attempt.outcomes.is_empty()
            || !attempt.failures.is_empty()
            || !attempt.cancellations.is_empty()
    };
    let (source, checkpoint) = match request {
        crate::exec::WorkRequest::Retry { .. } => {
            match &spec.resume {
                crate::exec::Resume::Restart => {}
                crate::exec::Resume::Checkpoint { .. } => {
                    return Err(crate::exec::WorkRefusal::Unsupported(
                        "this Run requires a committed checkpoint; use resume",
                    ));
                }
                crate::exec::Resume::Replay { .. } => {
                    return Err(crate::exec::WorkRefusal::Unsupported(
                        "this Run requires replay scheduling, which is not available yet",
                    ));
                }
                crate::exec::Resume::Never => {
                    return Err(crate::exec::WorkRefusal::Unsupported(
                        "this Run's Spec does not permit another Attempt",
                    ));
                }
            }
            let source = run
                .attempts
                .iter()
                .filter(terminal)
                .max_by_key(|attempt| (attempt.fence, attempt.leased_event))
                .ok_or(crate::exec::WorkRefusal::Unsupported(
                    "this Run has no completed Attempt whose scheduling coordinates can be continued",
                ))?;
            (source, None)
        }
        crate::exec::WorkRequest::Resume { checkpoint, .. } => {
            if !matches!(spec.resume, crate::exec::Resume::Checkpoint { .. }) {
                return Err(crate::exec::WorkRefusal::Unsupported(
                    "this Run restarts rather than resuming from a checkpoint; use continue",
                ));
            }
            let (source, checkpoint) = run
                .attempts
                .iter()
                .filter(terminal)
                .flat_map(|attempt| {
                    attempt
                        .checkpoints
                        .iter()
                        .filter(move |fact| fact.value.checkpoint.content == *checkpoint)
                        .map(move |fact| (attempt, fact))
                })
                .max_by_key(|(_, fact)| (fact.value.checkpoint.sequence, fact.event))
                .ok_or(crate::exec::WorkRefusal::Unsupported(
                    "that checkpoint is not a committed checkpoint of a completed Attempt on this Run",
                ))?;
            (source, Some(checkpoint.value.checkpoint.clone()))
        }
        crate::exec::WorkRequest::Inspect { .. }
        | crate::exec::WorkRequest::Watch { .. }
        | crate::exec::WorkRequest::Cancel { .. } => {
            return Err(crate::exec::WorkRefusal::Unsupported(
                "this Work action does not create an Attempt",
            ));
        }
    };
    if source.station != ambient.principal.station || source.station_epoch != ambient.epoch {
        return Err(crate::exec::WorkRefusal::Unsupported(
            "the prior Attempt belongs to another Station activation; a fresh scheduler decision is required",
        ));
    }
    if source.lease.is_some() {
        return Err(crate::exec::WorkRefusal::Unsupported(
            "service-backed work requires a renewed Role lease before it can continue",
        ));
    }
    let fence = run
        .attempts
        .iter()
        .map(|attempt| attempt.fence.as_u64())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|fence| *fence != 0)
        .ok_or(crate::exec::WorkRefusal::Unsupported(
            "the Run's fencing epoch is exhausted",
        ))?;
    Ok(crate::exec::Try {
        run: run_id,
        build: run.started.build,
        offer: source.offer.map(|id| crate::exec::OfferRef {
            id,
            station: source.station.clone(),
            station_epoch: source.station_epoch,
            epoch: source.offer_epoch,
        }),
        resources: source.resources.clone(),
        enforcement: source.enforcement,
        limits: source.limits,
        lease: None,
        checkpoint,
        fence: crate::exec::Fence::from_u64(fence),
    })
}

fn flatten_all(demand: AuthorizationDemand, output: &mut Vec<AuthorizationDemand>) {
    match demand {
        AuthorizationDemand::All(children) => {
            for child in children {
                flatten_all(child, output);
            }
        }
        other => output.push(other),
    }
}

fn combine_demands(world: &[u8], exec: &[Vec<u8>]) -> Result<Vec<u8>, Rejection> {
    if world.is_empty() {
        return Err(Rejection::ContractViolation);
    }
    combine_demand_set(Some(world), exec)
}

fn combine_exec_demands(exec: &[Vec<u8>]) -> Result<Vec<u8>, Rejection> {
    combine_demand_set(None, exec)
}

fn combine_demand_set(world: Option<&[u8]>, exec: &[Vec<u8>]) -> Result<Vec<u8>, Rejection> {
    let mut components = Vec::new();
    if let Some(world) = world {
        let world = AuthorizationDemand::decode_canonical(world)
            .map_err(|_| Rejection::ContractViolation)?;
        flatten_all(world, &mut components);
    }
    for demand in exec {
        let demand = AuthorizationDemand::decode_canonical(demand)
            .map_err(|_| Rejection::ContractViolation)?;
        flatten_all(demand, &mut components);
    }
    let mut canonical = components
        .into_iter()
        .map(|demand| {
            demand
                .encode_canonical()
                .map(|bytes| (bytes, demand))
                .map_err(|_| Rejection::ContractViolation)
        })
        .collect::<Result<Vec<_>, _>>()?;
    canonical.sort_by(|left, right| left.0.cmp(&right.0));
    canonical.dedup_by(|left, right| left.0 == right.0);
    if canonical.len() > mechanics::authorization::MAX_CHILDREN {
        return Err(Rejection::LimitExceeded);
    }
    let mut demands = canonical.into_iter().map(|(_, demand)| demand);
    let first = demands.next().ok_or(Rejection::ContractViolation)?;
    let demand = match demands.next() {
        None => first,
        Some(second) => AuthorizationDemand::All(
            std::iter::once(first)
                .chain(std::iter::once(second))
                .chain(demands)
                .collect(),
        ),
    };
    demand
        .encode_canonical()
        .map_err(|_| Rejection::LimitExceeded)
}

fn commit_failure(error: replica::transaction::commit::Failure) -> Failure {
    match error {
        replica::transaction::commit::Failure::UnsupportedOp => {
            Failure::Rejected(Rejection::ContractViolation)
        }
        replica::transaction::commit::Failure::PathInvalid => {
            tracing::warn!("World staged an invalid Body path");
            Failure::Rejected(Rejection::InvalidRequest)
        }
        replica::transaction::commit::Failure::InvalidOp(invalid) => {
            tracing::warn!(?invalid, "World staged an invalid Body operation");
            Failure::Rejected(Rejection::InvalidRequest)
        }
        replica::transaction::commit::Failure::OpLimit
        | replica::transaction::commit::Failure::EffectTooLarge
        | replica::transaction::commit::Failure::QuotaExceeded
        | replica::transaction::commit::Failure::OpaqueQuotaExceeded => {
            Failure::Rejected(Rejection::LimitExceeded)
        }
        replica::transaction::commit::Failure::TypeConflict
        | replica::transaction::commit::Failure::ParentManifestUnavailable => {
            Failure::Conflict(Conflict::Body)
        }
        replica::transaction::commit::Failure::SchemaMismatch => {
            Failure::Rejected(Rejection::ContractViolation)
        }
        replica::transaction::commit::Failure::RequestIdConflict => {
            Failure::Conflict(Conflict::Request)
        }
        replica::transaction::commit::Failure::Unauthorized(refusal) => {
            use mechanics::authorization::{DenialReason, Refusal};
            match refusal {
                Refusal::Denied(DenialReason::DemandUnsatisfied) => {
                    Failure::Rejected(Rejection::Denied(DeniedCause::DemandUnsatisfied))
                }
                Refusal::Denied(DenialReason::DeviceUnbound) => {
                    Failure::Rejected(Rejection::Denied(DeniedCause::NotAMember))
                }
                Refusal::Denied(DenialReason::ActorMismatch) => {
                    Failure::Rejected(Rejection::Denied(DeniedCause::PrincipalMismatch))
                }
                Refusal::Denied(DenialReason::Internal(what)) => {
                    tracing::warn!(what, "authorizer internal precondition failed");
                    Failure::Persistence
                }
                Refusal::ImplementationNotActive => {
                    Failure::Rejected(Rejection::NoActiveImplementation)
                }
                Refusal::Demand(invalid) => {
                    tracing::warn!(?invalid, "the World staged a malformed demand");
                    Failure::Rejected(Rejection::ContractViolation)
                }
                Refusal::Ledger(failure) => Failure::AuthorityUnavailable(format!("{failure:?}")),
            }
        }
        replica::transaction::commit::Failure::IntegrityCause {
            operation, reason, ..
        } => Failure::PersistenceCause { operation, reason },
        replica::transaction::commit::Failure::Illegitimate(_)
        | replica::transaction::commit::Failure::IllegitimateContact { .. }
        | replica::transaction::commit::Failure::Engine(_)
        | replica::transaction::commit::Failure::Integrity(_)
        | replica::transaction::commit::Failure::Body(_)
        | replica::transaction::commit::Failure::CheckpointBackpressure
        | replica::transaction::commit::Failure::BodyKeyUnavailable
        | replica::transaction::commit::Failure::Durability(_)
        | replica::transaction::commit::Failure::OutcomeUnknown
        | replica::transaction::commit::Failure::Poisoned => Failure::Persistence,
    }
}

fn query_publication_failure(error: crate::find::Failure) -> Failure {
    match error {
        crate::find::Failure::Interrupted => Failure::Interrupted,
        crate::find::Failure::NoActiveImplementation => {
            Failure::Rejected(Rejection::NoActiveImplementation)
        }
        crate::find::Failure::ImplementationUnavailable => {
            Failure::Rejected(Rejection::ImplementationUnavailable)
        }
        crate::find::Failure::AuthorityUnavailable(detail) => Failure::AuthorityUnavailable(detail),
        crate::find::Failure::PublicationUnavailable | crate::find::Failure::PublicationExpired => {
            Failure::GenerationUnavailable
        }
        other => Failure::PersistenceCause {
            operation: "resolve exact World query publication",
            reason: format!("{other:?}"),
        },
    }
}

impl crate::world::BodyReader for ReplicaReader<'_> {
    fn read_body(&self, key: &BodyKey) -> Option<Vec<u8>> {
        world_readable(self.replica.binding(key)).then(|| self.replica.read(key))?
    }
    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        if !world_readable(self.replica.binding(key)) {
            return Err(fabric::projection::Failure::NotCollaborative);
        }
        self.replica.read_collaborative(key)
    }
    fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        world_readable(self.replica.binding(key)).then(|| self.replica.body_version(key))?
    }
    fn anchor_in_body(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        world_readable(self.replica.binding(key))
            .then(|| self.replica.anchor(key, path, position))?
    }
    fn resolve_anchor(&self, key: &BodyKey, anchor: &fabric::Anchor) -> fabric::AnchorResolution {
        if !world_readable(self.replica.binding(key)) {
            return fabric::AnchorResolution::Drifted;
        }
        self.replica.resolve_anchor(key, anchor)
    }
    fn content_status(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<crate::world::ContentStatus> {
        // Residency is the host's question, not the Replica's, so a World
        // reading through a committed snapshot sees geometry with zero
        // residency. The host surface is where "how much is here" is answered,
        // because that is where the cache is.
        self.replica
            .content_descriptor(content)
            .map(|d| crate::world::ContentStatus {
                plaintext_len: d.plaintext_len,
                chunk_count: d.chunk_count,
                resident_chunks: 0,
            })
    }
    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        if crate::exec::is_reserved_schema(schema) {
            return Vec::new();
        }
        self.snapshot.body_keys_with_schema(world, schema)
    }
    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        if crate::exec::is_reserved_schema(schema) {
            return Vec::new();
        }
        self.snapshot
            .body_keys_page_with_schema(world, schema, after, limit)
    }
    fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        world_readable(self.replica.binding(key)).then(|| self.replica.body_stamp(key))?
    }
    fn outcome(
        &self,
        world: &WorldId,
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
    ) -> Option<crate::world::OutcomeFacts> {
        crate::exec::outcome_facts(self.snapshot, world, run, attempt)
            .ok()
            .flatten()
    }
}

/// A [`BodyReader`] over an immutable generation. Unlike [`ReplicaReader`],
/// this owns no borrow of the Station writer and is safe to evaluate after the
/// mutex has been released.
struct SnapshotReader(Arc<replica::ReadSnapshot>);

impl crate::world::BodyReader for SnapshotReader {
    fn read_body(&self, key: &BodyKey) -> Option<Vec<u8>> {
        world_readable(self.0.binding(key)).then(|| self.0.read(key))?
    }
    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        if !world_readable(self.0.binding(key)) {
            return Err(fabric::projection::Failure::NotCollaborative);
        }
        self.0.read_collaborative(key)
    }
    fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        world_readable(self.0.binding(key)).then(|| self.0.body_version(key))?
    }
    fn anchor_in_body(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        world_readable(self.0.binding(key)).then(|| self.0.anchor(key, path, position))?
    }
    fn resolve_anchor(&self, key: &BodyKey, anchor: &fabric::Anchor) -> fabric::AnchorResolution {
        if !world_readable(self.0.binding(key)) {
            return fabric::AnchorResolution::Drifted;
        }
        self.0.resolve_anchor(key, anchor)
    }
    fn content_status(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<crate::world::ContentStatus> {
        self.0
            .content_descriptor(content)
            .map(|descriptor| crate::world::ContentStatus {
                plaintext_len: descriptor.plaintext_len,
                chunk_count: descriptor.chunk_count,
                resident_chunks: 0,
            })
    }
    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        if crate::exec::is_reserved_schema(schema) {
            return Vec::new();
        }
        self.0.body_keys_with_schema(world, schema)
    }
    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        if crate::exec::is_reserved_schema(schema) {
            return Vec::new();
        }
        self.0
            .body_keys_page_with_schema(world, schema, after, limit)
    }
    fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        world_readable(self.0.binding(key)).then(|| self.0.body_stamp(key))?
    }
    fn outcome(
        &self,
        world: &WorldId,
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
    ) -> Option<crate::world::OutcomeFacts> {
        crate::exec::outcome_facts(&self.0, world, run, attempt)
            .ok()
            .flatten()
    }
}

#[allow(clippy::too_many_arguments)]
fn ensure_inner_world_publication(
    inner: &mut CoreInner,
    world: &Arc<dyn World>,
    world_id: &WorldId,
    implementation: [u8; 32],
    extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
    schemas: &[crate::find::Schema],
    extractors: &[crate::find::Extractor],
) -> Result<Arc<WorldPublication>, PublicationFailure> {
    let id = crate::publication::WorldPublicationId::new(
        crate::publication::PublicationId::new(
            inner.snapshot.root(),
            implementation,
            extractor_schema_digest,
        ),
        inner.snapshot_materialization,
    );
    if let Some(publication) = inner
        .world_publications
        .get(world_id)
        .filter(|publication| publication.id == id)
        .cloned()
    {
        return Ok(publication);
    }
    let snapshot = inner.snapshot.clone();
    let corpus = build_world_corpus(&snapshot, world, world_id, id, schemas, extractors)?;
    let publication = Arc::new(WorldPublication {
        id,
        snapshot,
        corpus: Arc::new(corpus),
    });
    inner.install_world_publication(world_id.clone(), publication.clone());
    Ok(publication)
}

fn build_world_corpus(
    snapshot: &Arc<replica::ReadSnapshot>,
    world: &Arc<dyn World>,
    world_id: &WorldId,
    id: crate::publication::WorldPublicationId,
    schemas: &[crate::find::Schema],
    extractors: &[crate::find::Extractor],
) -> Result<crate::corpus::Corpus, PublicationFailure> {
    let reader = SnapshotReader(snapshot.clone());
    let context = crate::world::ExtractionContext::new(&reader, world_id, id);
    let mut extracted = std::collections::BTreeMap::<BodyKey, crate::find::BodyExtraction>::new();

    for extractor in extractors {
        if !schemas.iter().any(|schema| {
            schema.reference == extractor.schema && schema.sources.contains(&extractor.source)
        }) {
            return Err(PublicationFailure::Extractor);
        }
        let bodies = <SnapshotReader as crate::world::BodyReader>::bodies_with_schema(
            &reader,
            world_id,
            &extractor.source.name,
        );
        for body in bodies {
            if snapshot
                .binding(&body)
                .is_none_or(|binding| binding.schema_version != extractor.source.version)
            {
                continue;
            }
            let mut output = std::panic::catch_unwind(AssertUnwindSafe(|| {
                world.extract(&context, extractor, &body)
            }))
            .map_err(|_| PublicationFailure::Extractor)?
            .map_err(|_| PublicationFailure::Extractor)?;
            if output.body != body
                || output
                    .nodes
                    .iter()
                    .any(|node| node.key.schema != extractor.schema)
            {
                return Err(PublicationFailure::Extractor);
            }
            match extracted.entry(body) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(output);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if entry.get().stamp != output.stamp {
                        return Err(PublicationFailure::Extractor);
                    }
                    entry.get_mut().nodes.append(&mut output.nodes);
                }
            }
        }
    }

    crate::corpus::Corpus::build(
        id,
        crate::corpus::Limits::default(),
        extracted.into_values().collect(),
    )
    .map(|(corpus, _)| corpus)
    .map_err(|_| PublicationFailure::Corpus)
}

#[allow(clippy::too_many_arguments)]
fn advance_inner_world_publication(
    inner: &mut CoreInner,
    prior: Option<Arc<WorldPublication>>,
    world: &Arc<dyn World>,
    world_id: &WorldId,
    implementation: [u8; 32],
    extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
    schemas: &[crate::find::Schema],
    extractors: &[crate::find::Extractor],
    changed: &[BodyKey],
) -> Result<(), PublicationFailure> {
    let id = crate::publication::WorldPublicationId::new(
        crate::publication::PublicationId::new(
            inner.snapshot.root(),
            implementation,
            extractor_schema_digest,
        ),
        inner.snapshot_materialization,
    );
    let publication = candidate_world_publication(
        inner.snapshot.clone(),
        id,
        prior,
        world,
        world_id,
        implementation,
        extractor_schema_digest,
        schemas,
        extractors,
        changed,
    )?;
    inner.install_world_publication(world_id.clone(), publication);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn candidate_world_publication(
    snapshot: Arc<replica::ReadSnapshot>,
    id: crate::publication::WorldPublicationId,
    prior: Option<Arc<WorldPublication>>,
    world: &Arc<dyn World>,
    world_id: &WorldId,
    implementation: [u8; 32],
    extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
    schemas: &[crate::find::Schema],
    extractors: &[crate::find::Extractor],
    changed: &[BodyKey],
) -> Result<Arc<WorldPublication>, PublicationFailure> {
    let corpus = if let Some(prior) = prior.filter(|prior| {
        prior.id.publication.implementation_digest == implementation
            && prior.id.publication.extractor_schema_digest == extractor_schema_digest
    }) {
        let bodies = extract_changed_bodies(
            &snapshot,
            &prior.corpus,
            id,
            world,
            world_id,
            schemas,
            extractors,
            changed,
        )?;
        prior
            .corpus
            .apply(crate::corpus::CorpusDelta {
                base: prior.id,
                next: id,
                bodies,
            })
            .map(|(corpus, _)| corpus)
            .map_err(|_| PublicationFailure::Corpus)?
    } else {
        build_world_corpus(&snapshot, world, world_id, id, schemas, extractors)?
    };
    Ok(Arc::new(WorldPublication {
        id,
        snapshot,
        corpus: Arc::new(corpus),
    }))
}

fn extract_changed_bodies(
    snapshot: &Arc<replica::ReadSnapshot>,
    prior: &crate::corpus::Corpus,
    id: crate::publication::WorldPublicationId,
    world: &Arc<dyn World>,
    world_id: &WorldId,
    schemas: &[crate::find::Schema],
    extractors: &[crate::find::Extractor],
    changed: &[BodyKey],
) -> Result<Vec<crate::find::BodyExtraction>, PublicationFailure> {
    let reader = SnapshotReader(snapshot.clone());
    let context = crate::world::ExtractionContext::new(&reader, world_id, id);
    let mut outputs = Vec::new();
    for body in changed.iter().filter(|body| &body.world == world_id) {
        let mut combined = crate::find::BodyExtraction {
            body: body.clone(),
            stamp: <SnapshotReader as crate::world::BodyReader>::body_stamp(&reader, body)
                .unwrap_or_default(),
            nodes: Vec::new(),
        };
        let binding = snapshot.binding(body);
        let matching: Vec<_> = extractors
            .iter()
            .filter(|extractor| {
                binding.is_some_and(|binding| {
                    binding.schema == extractor.source.name
                        && binding.schema_version == extractor.source.version
                })
            })
            .collect();
        if matching.is_empty() && prior.body_stamp(body).is_none() {
            continue;
        }
        for extractor in matching {
            if !schemas.iter().any(|schema| {
                schema.reference == extractor.schema && schema.sources.contains(&extractor.source)
            }) {
                return Err(PublicationFailure::Extractor);
            }
            let mut output = std::panic::catch_unwind(AssertUnwindSafe(|| {
                world.extract(&context, extractor, body)
            }))
            .map_err(|_| PublicationFailure::Extractor)?
            .map_err(|_| PublicationFailure::Extractor)?;
            if output.body != *body
                || output.stamp != combined.stamp
                || output
                    .nodes
                    .iter()
                    .any(|node| node.key.schema != extractor.schema)
            {
                return Err(PublicationFailure::Extractor);
            }
            combined.nodes.append(&mut output.nodes);
        }
        outputs.push(combined);
    }
    Ok(outputs)
}

/// Runtime-derived request coordinates shared by durable submit and Find.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ambient {
    epoch: Epoch,
    space: mechanics::ids::SpaceId,
    world: WorldId,
    implementation: [u8; 32],
    root: [u8; 32],
    extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
    principal: PrincipalFacts,
    find_policy: crate::find::Policy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AmbientFailure {
    NoActiveImplementation,
    ImplementationUnavailable,
    AuthorityUnavailable(String),
}

/// A local caller's handle to a hosted World.
pub struct Session {
    space: mechanics::ids::SpaceId,
    world_id: WorldId,
    world: Arc<dyn World>,
    /// Exact reviewed identity of `world`. A Session never invokes this code
    /// after authority activates a different implementation.
    implementation: [u8; 32],
    /// The docked identity: signs this Session's durable Body transactions.
    identity: crate::world::LocalIdentity,
    principal: PrincipalFacts,
    epoch: Epoch,
    /// The World's declared limits, enforced before the callback runs.
    limits: Limits,
    /// The World's declared schemas, checked against each request.
    schemas: Vec<Schema>,
    /// Canonical identity of the declarations that produce this World's
    /// shared query corpus.
    extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
    /// Local Find ceilings inherited from the Station activation.
    find_policy: crate::find::Policy,
    /// A shared flag: `false` once the Station is going dormant or has exited.
    /// A Session only *reads* it — it can never stop the Station.
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The Station's exclusive committing state.
    core: Arc<StationCore>,
    /// The mechanics authority view: standing is re-resolved per request and the
    /// authority frontier is compare-and-swapped at commit.
    authority: Arc<dyn AuthorityView>,
    /// All installed exact World packages. Durable writes remain bound to
    /// `world`; historical Find uses this catalog to interpret a retained root
    /// with the implementation named by its PublicationId.
    registry: crate::registry::Catalog,
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        space: mechanics::ids::SpaceId,
        world_id: WorldId,
        world: Arc<dyn World>,
        identity: crate::world::LocalIdentity,
        principal: PrincipalFacts,
        epoch: Epoch,
        limits: Limits,
        schemas: Vec<Schema>,
        implementation: [u8; 32],
        extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
        find_policy: crate::find::Policy,
        alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
        core: Arc<StationCore>,
        authority: Arc<dyn AuthorityView>,
        registry: crate::registry::Catalog,
    ) -> Self {
        Self {
            space,
            world_id,
            world,
            implementation,
            identity,
            principal,
            epoch,
            limits,
            schemas,
            extractor_schema_digest,
            find_policy,
            alive,
            core,
            authority,
            registry,
        }
    }

    /// The Space this Session's Station serves.
    pub fn space_id(&self) -> &mechanics::ids::SpaceId {
        &self.space
    }

    /// Resolve fresh mechanics facts for `sign_action` — only the docked
    /// device resolves through this Session's authority view.
    pub(crate) fn resolve_for_signing(
        &self,
        device: &mechanics::ids::DeviceId,
    ) -> Option<crate::world::PrincipalResolution> {
        if device != &self.principal.device {
            return None;
        }
        self.authority.resolve(device)
    }

    /// Fresh principal facts for THIS request: standing and the authority
    /// frontier are re-resolved through the mechanics view, so dock-time facts
    /// never outlive the authority state. Denied when the device no longer
    /// resolves.
    fn fresh_principal(&self) -> Result<PrincipalFacts, Rejection> {
        let resolution = self
            .authority
            .resolve(&self.principal.device)
            .ok_or(Rejection::Denied(DeniedCause::NotAMember))?;
        Ok(PrincipalFacts {
            actor: resolution.actor,
            device: self.principal.device.clone(),
            station: self.principal.station.clone(),
            space: self.space.clone(),
            authority_frontier: resolution.authority_frontier,
        })
    }

    fn context_find_gates(
        &self,
        principal: &PrincipalFacts,
    ) -> Result<crate::find_evaluator::GrantedGates, Failure> {
        self.context_find_gates_for(principal, self.world.find_schemas())
    }

    fn context_find_gates_for(
        &self,
        principal: &PrincipalFacts,
        schemas: &[crate::find::Schema],
    ) -> Result<crate::find_evaluator::GrantedGates, Failure> {
        let mut granted = std::collections::BTreeSet::new();
        for schema in schemas {
            for gate in &schema.gates {
                match self.authority.evaluate_read(
                    &principal.actor,
                    &principal.authority_frontier,
                    &gate.demand,
                ) {
                    Ok(true) => {
                        granted.insert(gate.reference.clone());
                    }
                    Ok(false) => {}
                    Err(detail) => return Err(Failure::AuthorityUnavailable(detail)),
                }
            }
        }
        Ok(crate::find_evaluator::GrantedGates::new(granted))
    }

    /// Derive the complete ambient request prefix from Runtime-owned state.
    /// Callers supply neither these coordinates nor Station policy.
    fn ambient(
        &self,
        principal: &PrincipalFacts,
        root: [u8; 32],
    ) -> Result<Ambient, AmbientFailure> {
        let implementation = self
            .authority
            .active_implementation(&self.world_id, &principal.authority_frontier)
            .map_err(AmbientFailure::AuthorityUnavailable)?
            .ok_or(AmbientFailure::NoActiveImplementation)?;
        if implementation != self.implementation {
            return Err(AmbientFailure::ImplementationUnavailable);
        }
        Ok(Ambient {
            epoch: self.epoch,
            space: self.space.clone(),
            world: self.world_id.clone(),
            implementation,
            root,
            extractor_schema_digest: self.extractor_schema_digest,
            principal: principal.clone(),
            find_policy: self.find_policy,
        })
    }

    /// Contain a World's staged effect inside its own namespace and each
    /// staged Body's **exact schema binding** — not merely "any model the
    /// World registered". Every operation resolves a binding: an existing
    /// Body's recorded (immutable) binding, an explicit create declaration, or
    /// — for a new Body with no declaration — the intent's schema. The binding
    /// must be a registered, writable schema of this World, and the operation
    /// family must match its mutation model. Returns the per-Body bindings the
    /// commit is made under.
    fn contain_effect(
        &self,
        replica: &replica::Replica,
        effect: &Effect,
        intent_schema: &SchemaId,
        runtime: &RuntimeEffect,
    ) -> Result<Vec<(BodyKey, replica::body::BodyBinding)>, Rejection> {
        validate_operation_partition(&self.world_id, effect, runtime)?;
        if effect.declarations.iter().any(|declaration| {
            crate::exec::is_reserved_schema(&declaration.schema)
                || declaration.key.world != self.world_id
        }) || effect
            .operations
            .iter()
            .map(|(key, _)| key)
            .chain(effect.content_refs.iter().map(|(key, _)| key))
            .chain(effect.bodies.iter())
            .any(|key| {
                key.world != self.world_id
                    || replica
                        .binding(key)
                        .is_some_and(|binding| crate::exec::is_reserved_schema(&binding.schema))
            })
        {
            return Err(Rejection::ContractViolation);
        }
        let mut bindings: Vec<(BodyKey, replica::body::BodyBinding)> = Vec::new();
        for (key, op) in &effect.operations {
            if key.world != self.world_id {
                return Err(Rejection::ContractViolation);
            }
            // Resolve the Body's schema binding.
            let (schema_id, version) = if let Some(existing) = replica.binding(key) {
                // Existing Body: its binding is immutable; a declaration that
                // disagrees is a violation.
                if let Some(d) = effect.declarations.iter().find(|d| &d.key == key) {
                    if d.schema != existing.schema || d.schema_version != existing.schema_version {
                        return Err(Rejection::ContractViolation);
                    }
                }
                (existing.schema.clone(), existing.schema_version)
            } else if let Some(d) = effect.declarations.iter().find(|d| &d.key == key) {
                (d.schema.clone(), d.schema_version)
            } else {
                (intent_schema.clone(), self.intent_version(intent_schema)?)
            };
            let schema = self
                .schemas
                .iter()
                .find(|s| s.id == schema_id && s.version == version)
                .ok_or_else(|| {
                    tracing::warn!(
                        body = ?key,
                        schema = %schema_id,
                        version,
                        "World effect targeted a Body whose exact schema binding is not writable"
                    );
                    Rejection::ContractViolation
                })?;
            let collaborative = matches!(schema.mutation, MutationModel::Collaborative(_));
            let permitted = match op {
                Op::ReplaceAtomic { .. } => !collaborative,
                Op::Create => collaborative,
                Op::Tombstone => true,
                _ => collaborative,
            };
            if !permitted {
                return Err(Rejection::ContractViolation);
            }
            if !bindings.iter().any(|(k, _)| k == key) {
                bindings.push((
                    key.clone(),
                    replica::body::BodyBinding {
                        schema: schema.id.clone(),
                        schema_version: schema.version,
                        encoding: schema.encoding.clone(),
                        mutation_model: if collaborative {
                            replica::body::MUTATION_COLLABORATIVE
                        } else {
                            replica::body::MUTATION_ATOMIC
                        },
                    },
                ));
            }
        }
        for scope in &effect.bodies {
            if scope.world != self.world_id {
                return Err(Rejection::ContractViolation);
            }
        }
        for (key, binding) in &runtime.bindings {
            if key.world != self.world_id
                || binding.schema.as_str() != crate::exec::RUN_BODY_SCHEMA
                || binding.schema_version != crate::exec::RUN_BODY_SCHEMA_VERSION
                || binding.encoding.as_str() != crate::exec::BODY_ENCODING
                || binding.mutation_model != replica::body::MUTATION_COLLABORATIVE
                || replica
                    .binding(key)
                    .is_some_and(|existing| existing != binding)
            {
                return Err(Rejection::ContractViolation);
            }
            bindings.push((key.clone(), binding.clone()));
        }
        Ok(bindings)
    }

    /// The registered version of the intent schema (validated writable before
    /// the callback ran).
    fn intent_version(&self, schema: &SchemaId) -> Result<u32, Rejection> {
        self.schemas
            .iter()
            .find(|s| &s.id == schema)
            .map(|s| s.version)
            .ok_or(Rejection::ContractViolation)
    }

    fn ensure_live(&self) -> Result<(), Failure> {
        if self.alive.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Failure::Interrupted)
        }
    }

    /// Enforce the declared payload limit (a limit of `0` means "Runtime
    /// default", currently unbounded — S1 freezes the real default).
    fn ensure_within_limit(&self, payload_len: usize) -> Result<(), Rejection> {
        let max = self.limits.max_payload_bytes;
        if max != 0 && payload_len > max as usize {
            return Err(Rejection::LimitExceeded);
        }
        Ok(())
    }

    /// The exact `(schema, version)` must be a declared, writable schema.
    fn ensure_writable_schema(&self, schema: &SchemaId, version: u32) -> Result<(), Rejection> {
        let known = self.schemas.iter().find(|s| &s.id == schema);
        match known {
            None => Err(Rejection::UnsupportedSchema),
            Some(s) if s.version == version => Ok(()),
            Some(_) => Err(Rejection::UnsupportedSchemaVersion),
        }
    }

    /// A query may read the declared version or any of its readable predecessors.
    fn ensure_readable_schema(&self, schema: &SchemaId, version: u32) -> Result<(), Rejection> {
        Self::ensure_readable_schema_in(&self.schemas, schema, version)
    }

    fn ensure_readable_schema_in(
        schemas: &[Schema],
        schema: &SchemaId,
        version: u32,
    ) -> Result<(), Rejection> {
        let mut saw_schema = false;
        for s in schemas {
            if &s.id != schema {
                continue;
            }
            saw_schema = true;
            if s.version == version || s.readable_predecessors.contains(&version) {
                return Ok(());
            }
        }
        if saw_schema {
            Err(Rejection::UnsupportedSchemaVersion)
        } else {
            Err(Rejection::UnsupportedSchema)
        }
    }

    /// The World this Session is docked to.
    pub fn world_id(&self) -> &WorldId {
        &self.world_id
    }

    /// The Station activation epoch this Session is bound to.
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// The Station this Session is docked to.
    pub fn station(&self) -> &mechanics::station::Key {
        &self.principal.station
    }

    /// The exact World implementation this Session may invoke.
    pub fn implementation(&self) -> [u8; 32] {
        self.implementation
    }

    /// Record signed, expiring Offer news for this Station activation.
    ///
    /// The news is lossy: it is not a reserved Body and it reserves no Run. A
    /// later [`crate::exec::Cmd::Try`] may cite it as evidence. Station-only
    /// first Attempts omit it.
    pub fn announce(&self, offer: crate::exec::Offer) -> Result<crate::exec::OfferId, Failure> {
        self.ensure_live()?;
        let principal = self.fresh_principal()?;
        let now = mechanics::wallclock::now_millis();
        offer.validate().map_err(exec_invalid)?;
        offer.usable_at(now).map_err(exec_invalid)?;
        if offer.space != self.space
            || offer.station != principal.station
            || offer.station_epoch != self.epoch
            || offer.actor != principal.actor
            || offer.device != principal.device
            || offer.world != self.world_id
            || offer.world_build != self.implementation
        {
            return Err(Rejection::ContractViolation.into());
        }
        let specs = self.world.exec_specs();
        let mut offer_demands = Vec::new();
        for claimed in &offer.builds {
            let spec = specs
                .iter()
                .find(|spec| spec.name == claimed.spec.name && spec.version == claimed.spec.version)
                .ok_or(Rejection::ContractViolation)?;
            offer_demands.push(spec.access.offer.clone());
        }
        let demand = combine_exec_demands(&offer_demands)?;
        let mut inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        let intent_digest = *blake3::hash(&offer.id.as_bytes()).as_bytes();
        self.authority
            .authorize_mutation(
                &self.space,
                &self.world_id,
                &principal.actor,
                &principal.device,
                &principal.authority_frontier,
                inner.snapshot.root(),
                self.implementation,
                intent_digest,
                &demand,
                [0; 32],
                intent_digest,
            )
            .map_err(|_| Rejection::Denied(DeniedCause::DemandUnsatisfied))?;
        prune_offer_news(&mut inner, now);
        if !inner.offers.contains_key(&offer.id)
            && inner.offers.len() >= crate::exec::MAX_OFFERS_PER_STATION
        {
            return Err(Rejection::LimitExceeded.into());
        }
        let id = offer.id;
        inner.offers.insert(id, offer);
        Ok(id)
    }

    /// Return one piece of still-held Offer news, if this activation has it.
    pub fn news(&self, id: crate::exec::OfferId) -> Option<crate::exec::Offer> {
        let inner = self.core.lock();
        inner.offers.get(&id).cloned()
    }

    /// Issue a nonce-bound readiness challenge against live Offer news.
    ///
    /// The challenge expires on its own clock and never reserves a Run.
    pub fn challenge(
        &self,
        offer: crate::exec::OfferId,
    ) -> Result<crate::exec::Challenge, Failure> {
        self.ensure_live()?;
        let principal = self.fresh_principal()?;
        let now = mechanics::wallclock::now_millis();
        let mut inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        prune_offer_news(&mut inner, now);
        let held = inner
            .offers
            .get(&offer)
            .ok_or(Rejection::ContractViolation)?;
        held.usable_at(now).map_err(exec_invalid)?;
        if held.station != principal.station
            || held.station_epoch != self.epoch
            || held.device != principal.device
        {
            return Err(Rejection::ContractViolation.into());
        }
        if inner.challenges.len() >= crate::exec::MAX_CHALLENGES_PER_STATION {
            return Err(Rejection::LimitExceeded.into());
        }
        let ttl = crate::exec::CHALLENGE_TTL_MILLIS.min(held.expiry.saturating_sub(now));
        if ttl == 0 {
            return Err(Rejection::ContractViolation.into());
        }
        let challenge = crate::exec::Challenge {
            offer,
            nonce: crate::action::RequestId::mint().as_bytes(),
            station: principal.station,
            station_epoch: self.epoch,
            issued_at: now.max(1),
            expiry: now.saturating_add(ttl),
        };
        challenge.validate().map_err(exec_invalid)?;
        inner
            .challenges
            .insert((challenge.offer, challenge.nonce), challenge.clone());
        Ok(challenge)
    }

    /// Accept a signed Ready answer to an outstanding challenge.
    ///
    /// Readiness is not intent and does not own the Run. An answered challenge
    /// is consumed; a later first-use Try may consume the Ready.
    pub fn ready(&self, answer: crate::exec::Ready) -> Result<crate::exec::OfferId, Failure> {
        self.ensure_live()?;
        let principal = self.fresh_principal()?;
        let now = mechanics::wallclock::now_millis();
        let mut inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        prune_offer_news(&mut inner, now);
        let challenge = inner
            .challenges
            .remove(&(answer.offer, answer.nonce))
            .ok_or(Rejection::ContractViolation)?;
        challenge.usable_at(now).map_err(exec_invalid)?;
        answer.validate_for(&challenge).map_err(exec_invalid)?;
        if answer.signature.signer != principal.device
            || challenge.station != principal.station
            || challenge.station_epoch != self.epoch
        {
            return Err(Rejection::ContractViolation.into());
        }
        let id = answer.offer;
        inner.readies.insert(
            id,
            AcceptedReady {
                challenge,
                ready: answer,
            },
        );
        Ok(id)
    }

    /// Publish one signed Build envelope into the reserved Build Body.
    ///
    /// This is durable identity, not a ranking of Builds. Republishing the
    /// exact same envelope is idempotent. A different envelope under the same
    /// Build id is refused. Current/ramping choice is not decided here.
    pub fn publish_build(
        &self,
        build: crate::exec::Build,
    ) -> Result<crate::exec::BuildId, Failure> {
        self.ensure_live()?;
        let principal = self.fresh_principal()?;
        build.validate().map_err(exec_invalid)?;
        if build.world != self.world_id || build.world_build != self.implementation {
            return Err(Rejection::ContractViolation.into());
        }
        let spec = self
            .world
            .exec_specs()
            .iter()
            .find(|spec| spec.name == build.spec.name && spec.version == build.spec.version)
            .ok_or(Rejection::ContractViolation)?;
        let envelope = build.encode().map_err(exec_invalid)?;
        let id = build.id;
        let key = BodyKey {
            world: self.world_id.clone(),
            body: crate::exec::derive_build_body_id(id),
        };
        let mut inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        if let Ok(view) = inner.snapshot.read_collaborative(&key) {
            let existing = view
                .maps
                .get(crate::exec::BUILD_ENVELOPE_PATH)
                .and_then(|entries| entries.get(crate::exec::BUILD_ENVELOPE_KEY));
            if existing == Some(&envelope) {
                return Ok(id);
            }
            return Err(Rejection::ContractViolation.into());
        }
        let mut runtime = RuntimeEffect::default();
        runtime.operations.push((key.clone(), Op::Create));
        runtime.operations.push((
            key.clone(),
            Op::MapSet {
                path: crate::exec::BUILD_ENVELOPE_PATH.to_string(),
                key: crate::exec::BUILD_ENVELOPE_KEY.to_string(),
                value: envelope,
            },
        ));
        runtime.bindings.push((key.clone(), build_binding()?));
        runtime.bodies.push(key.clone());
        runtime.demands.push(spec.access.control.clone());
        let ambient = self
            .ambient(&principal, inner.snapshot.root())
            .map_err(ambient_failure)?;
        let operation = crate::action::RequestId::mint().as_bytes();
        let digest = *blake3::hash(&build.id.as_bytes()).as_bytes();
        self.commit_runtime_effect(
            &mut inner,
            &principal,
            &ambient,
            operation,
            digest,
            "exec.build.publish",
            runtime,
        )?;
        Ok(id)
    }

    /// Read one published Build envelope from the reserved Body, if present.
    pub fn published_build(
        &self,
        id: crate::exec::BuildId,
    ) -> Result<Option<crate::exec::Build>, Failure> {
        self.ensure_live()?;
        let inner = self.core.lock();
        let key = BodyKey {
            world: self.world_id.clone(),
            body: crate::exec::derive_build_body_id(id),
        };
        let Ok(view) = inner.snapshot.read_collaborative(&key) else {
            return Ok(None);
        };
        let Some(bytes) = view
            .maps
            .get(crate::exec::BUILD_ENVELOPE_PATH)
            .and_then(|entries| entries.get(crate::exec::BUILD_ENVELOPE_KEY))
        else {
            return Ok(None);
        };
        let build = crate::exec::Build::decode_canonical(bytes).map_err(exec_invalid)?;
        if build.id != id {
            return Err(Rejection::ContractViolation.into());
        }
        Ok(Some(build))
    }

    #[cfg(test)]
    pub(crate) fn test_read_reserved_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        let inner = self.core.lock();
        inner.snapshot.read_collaborative(key)
    }

    /// Submit a canonical signed action and **durably commit** its effect under
    /// the persistent-idempotency scope `(Space, World, Device, RequestId)`.
    ///
    /// The action is verified (canonical form, payload binding, signer
    /// self-signature) and must name this Session's Space and World; the signer
    /// must be the docked principal, re-resolved through mechanics for this
    /// request; and the header's authority frontier must still be current at
    /// commit (a change returns [`Conflict::AuthorityChanged`]). An
    /// identical replay returns the original [`CommittedEffect`] without
    /// reapplying any operation; reusing the request id with a different
    /// payload returns [`Conflict::Request`]. A refused request commits
    /// nothing. The returned [`CommittedEffect`] is proof of durability: it
    /// exists only after the journaled store committed the transaction.
    pub fn submit(
        &self,
        action: crate::action::SignedWorldAction,
    ) -> Result<CommittedEffect, Failure> {
        self.ensure_live()?;
        // Opaque verification first: version, algorithm, bounds, payload hash,
        // signer identity, self-signature.
        action.verify_self().map_err(|e| match e {
            crate::action::Invalid::PayloadTooLarge => Failure::Rejected(Rejection::LimitExceeded),
            _ => Failure::Rejected(Rejection::InvalidRequest),
        })?;
        // The action must address exactly this Session.
        if action.header.space != self.space || action.header.world != self.world_id {
            return Err(Rejection::InvalidRequest.into());
        }
        let intent = Intent {
            schema: action.header.intent_schema.clone(),
            schema_version: action.header.intent_version,
            payload: action.payload,
        };
        self.ensure_within_limit(intent.payload.len())?;
        self.ensure_writable_schema(&intent.schema, intent.schema_version)?;
        let world = &self.world;
        let label = intent.schema.as_str().to_string();
        let intent_schema = intent.schema.clone();
        let request = action.header.request.as_bytes();
        let payload_hash = action.header.payload_hash;
        // Hold the exclusive writer across the WHOLE transaction — including
        // both authority resolutions. Authorization, the idempotency lookup,
        // the World callback, the frontier compare-and-swap, and the durable
        // commit all run inside one critical section, so any authority
        // mutation that itself serializes through this Station's writer (as
        // orbital authority mutations do — membership changes are Replica
        // commits) cannot interleave between the comparison and the commit.
        // External `AuthorityView` implementations owe the linearizable-read
        // contract documented on the trait.
        let mut inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        // Per-request authorization, resolved under the writer lock. The
        // signer must BE the docked principal.
        let principal = self.fresh_principal()?;
        if action.header.actor != principal.actor || action.header.device != principal.device {
            return Err(Rejection::Denied(DeniedCause::PrincipalMismatch).into());
        }
        // Idempotency: an identical replay returns the original committed
        // result before the World runs again; a conflicting reuse is refused.
        match inner.replica.lookup_action(
            &self.space,
            &self.world_id,
            &principal.device,
            &request,
            &payload_hash,
        ) {
            Ok(None) => {}
            Ok(Some(receipt)) => {
                let publication = inner.receipt_publication(&receipt)?;
                return Ok(CommittedEffect {
                    effect: receipt.effect,
                    frontier: receipt.frontier,
                    bodies: receipt.bodies,
                    publication,
                });
            }
            Err(replica::transaction::commit::Failure::RequestIdConflict) => {
                return Err(Failure::Conflict(Conflict::Request))
            }
            Err(_) => return Err(Failure::Persistence),
        }
        // The frontier the action was signed against must still be current —
        // the same compare the commit-side CAS re-checks after the callback.
        if action.header.authority_frontier != principal.authority_frontier {
            return Err(Failure::Conflict(Conflict::AuthorityChanged));
        }
        let ambient = self
            .ambient(&principal, inner.snapshot.root())
            .map_err(|failure| match failure {
                AmbientFailure::NoActiveImplementation => {
                    Failure::Rejected(Rejection::NoActiveImplementation)
                }
                AmbientFailure::ImplementationUnavailable => {
                    Failure::Rejected(Rejection::ImplementationUnavailable)
                }
                AmbientFailure::AuthorityUnavailable(detail) => {
                    Failure::AuthorityUnavailable(detail)
                }
            })?;
        let publication = ensure_inner_world_publication(
            &mut inner,
            &self.world,
            &self.world_id,
            ambient.implementation,
            ambient.extractor_schema_digest,
            self.world.find_schemas(),
            self.world.find_extractors(),
        )
        .map_err(|failure| Failure::PersistenceCause {
            operation: "prepare World query publication",
            reason: format!("{failure:?}"),
        })?;
        let find_gates = self.context_find_gates(&principal)?;
        let pinned = publication.snapshot.clone();
        let issued_find_cursor = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let effect: Effect = {
            let reader = ReplicaReader {
                replica: &inner.replica,
                snapshot: &pinned,
            };
            let principal = &principal;
            let find = crate::world::FindHandle::new(Arc::new(ContextFindReader {
                publication: publication.clone(),
                schemas: Arc::from(self.world.find_schemas()),
                policy: self.find_policy,
                gates: find_gates,
                epoch: self.epoch,
                space: self.space.clone(),
                world: self.world_id.clone(),
                implementation: self.implementation,
                actor: principal.actor.clone(),
                device: principal.device.clone(),
                authority_frontier: principal.authority_frontier.clone(),
                issued_cursor: issued_find_cursor.clone(),
            }));
            let decision = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let mut ctx = Context::with_world_submission(
                    principal,
                    &reader,
                    publication.id,
                    &self.world_id,
                    action.header.request,
                    find,
                );
                world.submit(&mut ctx, intent)
            }))
            .map_err(|_| Failure::CallbackPanicked)?;
            decision.map_err(Failure::Rejected)?
        };
        if issued_find_cursor.load(std::sync::atomic::Ordering::Acquire) {
            inner
                .lease_world_publication(self.world_id.clone(), publication.clone())
                .map_err(|_| Failure::PersistenceCause {
                    operation: "retain World callback Find cursor publication",
                    reason: "Station cursor retention capacity exceeded".into(),
                })?;
        }
        let now_millis = mechanics::wallclock::now_millis();
        let admission = OfferAdmission {
            news: &inner.offers,
            readies: &inner.readies,
            now_millis,
        };
        let runtime = lower_exec(
            &effect.exec,
            world.exec_specs(),
            &ambient,
            request,
            effect.operations.len(),
            &pinned,
            &admission,
        )
        .inspect_err(|rejection| {
            tracing::warn!(?rejection, "Runtime refused a staged Exec command");
        })?;
        // Contain the staged effect inside this World's namespace and each
        // Body's exact schema binding, resolving the bindings the commit is
        // made under.
        let bindings = self
            .contain_effect(&inner.replica, &effect, &intent_schema, &runtime)
            .inspect_err(|rejection| {
                tracing::warn!(?rejection, "World effect containment failed");
            })?;
        // Authority-frontier compare-and-swap, still under the writer lock:
        // the frontier the request was authorized at must still be current at
        // commit. A change refuses the commit with AuthorityChanged and
        // commits nothing.
        let current = self
            .authority
            .resolve(&principal.device)
            .ok_or(Rejection::Denied(DeniedCause::NotAMember))?;
        if current.authority_frontier != action.header.authority_frontier {
            return Err(Failure::Conflict(Conflict::AuthorityChanged));
        }
        // The transaction must satisfy the ordinary World mutation and every
        // staged command's independently declared Start demand. One canonical
        // conjunction is bound into the one signed transaction receipt.
        let demand = combine_demands(&effect.demand, &runtime.demands)?;
        let implementation_id = ambient.implementation;
        let parent_manifest_root = inner.replica.manifest_root();
        let ctx = replica::transaction::CommitContext {
            space: &self.space,
            signer: &self.identity,
            authority_frontier: action.header.authority_frontier.clone(),
        };
        // The authorizer produces the AuthorizationReceipt from the built core
        // digest, binding every companion coordinate. A denial is a typed Err.
        let authorizer = SessionAuthorizer {
            authority: self.authority.as_ref(),
            space: &self.space,
            world: &self.world_id,
            actor: &principal.actor,
            device: &principal.device,
            authority_frontier: &action.header.authority_frontier,
            implementation_id,
        };
        let auth = replica::transaction::CommitAuthorization {
            actor: principal.actor.as_str(),
            parent_manifest_root,
            demand,
            intent_digest: payload_hash,
            authorizer: &authorizer,
        };
        let mut operations = effect.operations;
        operations.extend(runtime.operations);
        let mut bodies = effect.bodies;
        bodies.extend(runtime.bodies);
        bodies.sort();
        bodies.dedup();
        let mut content_refs = effect.content_refs;
        content_refs.extend(runtime.content_refs);
        let mut durable_change = crate::change::DurableChange::from_operations(
            crate::change::Attribution {
                operation: request,
                actor: principal.actor.clone(),
                device: principal.device.clone(),
            },
            &operations,
        );
        durable_change.cover_bodies(bodies.iter().cloned());
        let prior_publication = inner.world_publications.get(&self.world_id).cloned();
        let prior = inner.snapshot.clone();
        let candidate_materialization = inner.next_materialization;
        let (receipt, fresh) = {
            let outcome = inner
                .replica
                .prepare_action(
                    &ctx,
                    &auth,
                    replica::receipt::Interpretation {
                        implementation_digest: self.implementation,
                        extractor_schema_digest: self.extractor_schema_digest.digest(),
                    },
                    &self.world_id,
                    &principal.device,
                    &request,
                    &payload_hash,
                    effect.effect,
                    bodies,
                    &label,
                    &operations,
                    &bindings,
                    &content_refs,
                )
                .map_err(commit_failure)?;
            match outcome {
                replica::transaction::PreparedActionOutcome::Replayed(receipt) => (receipt, None),
                replica::transaction::PreparedActionOutcome::Prepared(prepared) => {
                    let candidate_receipt = prepared.receipt().clone();
                    let snapshot = Arc::new(
                        prepared
                            .candidate_snapshot(&prior)
                            .map_err(commit_failure)?,
                    );
                    durable_change.stabilize_prepared(&operations, &snapshot);
                    let id = crate::publication::WorldPublicationId::new(
                        crate::publication::PublicationId::new(
                            snapshot.root(),
                            self.implementation,
                            self.extractor_schema_digest,
                        ),
                        candidate_materialization,
                    );
                    let publication = candidate_world_publication(
                        snapshot.clone(),
                        id,
                        prior_publication,
                        &self.world,
                        &self.world_id,
                        self.implementation,
                        self.extractor_schema_digest,
                        self.world.find_schemas(),
                        self.world.find_extractors(),
                        &candidate_receipt.bodies,
                    )
                    .map_err(|failure| Failure::PersistenceCause {
                        operation: "build candidate World publication",
                        reason: format!("{failure:?}"),
                    })?;

                    // Only after the exact corpus is ready do signed material,
                    // Manifest, generation, and receipt cross the journal commit
                    // point. A failed build above drops `prepared` and rolls the
                    // Fabric candidate back.
                    let receipt = prepared.finalize(&ctx).map_err(commit_failure)?;
                    debug_assert_eq!(receipt, candidate_receipt);
                    (receipt, Some((snapshot, publication)))
                }
            }
        };
        let committed_publication = match &fresh {
            Some((_, publication)) => publication.id,
            None => inner.receipt_publication(&receipt)?,
        };
        if let Some((snapshot, publication)) = fresh {
            // Publish the immutable snapshot + corpus and its Observation
            // while still holding the writer. Readers can observe the old
            // pair or the new pair, never a durable root with a stale
            // target-World corpus.
            let parent = prior.root();
            inner.publish_snapshot(snapshot, Some(parent), Some(&receipt.bodies));
            debug_assert_eq!(inner.snapshot_materialization, candidate_materialization);
            inner.install_world_publication(self.world_id.clone(), publication);
            durable_change.cover_bodies(receipt.bodies.iter().cloned());
            self.core.note_exec();
            self.core.broadcaster.publish_change(
                durable_change,
                receipt.frontier,
                false,
                vec![AffectedWorldPublication {
                    world: self.world_id.clone(),
                    publication: committed_publication,
                }],
            );
        }
        consume_first_use_readies(&mut inner, &effect.exec, &pinned, &ambient.world);
        drop(inner);
        Ok(CommittedEffect {
            effect: receipt.effect,
            frontier: receipt.frontier,
            bodies: receipt.bodies,
            publication: committed_publication,
        })
    }

    /// Inspect or control durable Exec lifecycle state without entering a
    /// product callback.
    ///
    /// The request is bound to this Session's World and exposes no Start path.
    /// Mutating operations pass through the same `lower_exec` validator used
    /// for `World::submit`, then authorize and commit the protected event under
    /// the acting Session identity. `operation` is the host-minted persistent
    /// idempotency coordinate for this one generic control action.
    pub fn work(
        &self,
        request: crate::exec::WorkRequest,
        operation: [u8; 16],
    ) -> Result<crate::exec::WorkReply, crate::exec::WorkRefusal> {
        self.ensure_live().map_err(crate::exec::WorkRefusal::from)?;
        request.validate()?;
        if request.world() != &self.world_id {
            return Err(crate::exec::WorkRefusal::Invalid(
                crate::exec::Invalid::InvalidEvent("work world"),
            ));
        }
        match &request {
            crate::exec::WorkRequest::Inspect { .. } | crate::exec::WorkRequest::Watch { .. } => {
                let inner = self.core.lock();
                if inner.closed {
                    return Err(crate::exec::WorkRefusal::Session(Failure::Interrupted));
                }
                self.fresh_principal()
                    .map_err(Failure::from)
                    .map_err(crate::exec::WorkRefusal::from)?;
                work_reply(&inner.snapshot, &request)
            }
            crate::exec::WorkRequest::Cancel { .. }
            | crate::exec::WorkRequest::Retry { .. }
            | crate::exec::WorkRequest::Resume { .. } => {
                let digest = request.digest()?;
                let mut inner = self.core.lock();
                if inner.closed {
                    return Err(crate::exec::WorkRefusal::Session(Failure::Interrupted));
                }
                let principal = self
                    .fresh_principal()
                    .map_err(Failure::from)
                    .map_err(crate::exec::WorkRefusal::from)?;
                match inner.replica.lookup_action(
                    &self.space,
                    &self.world_id,
                    &principal.device,
                    &operation,
                    &digest,
                ) {
                    Ok(None) => {}
                    Ok(Some(_)) => return work_reply(&inner.snapshot, &request),
                    Err(replica::transaction::commit::Failure::RequestIdConflict) => {
                        return Err(crate::exec::WorkRefusal::Session(Failure::Conflict(
                            Conflict::Request,
                        )));
                    }
                    Err(_) => {
                        return Err(crate::exec::WorkRefusal::Session(Failure::Persistence));
                    }
                }
                let ambient = self
                    .ambient(&principal, inner.snapshot.root())
                    .map_err(|failure| match failure {
                        AmbientFailure::NoActiveImplementation => {
                            Failure::Rejected(Rejection::NoActiveImplementation)
                        }
                        AmbientFailure::ImplementationUnavailable => {
                            Failure::Rejected(Rejection::ImplementationUnavailable)
                        }
                        AmbientFailure::AuthorityUnavailable(detail) => {
                            Failure::AuthorityUnavailable(detail)
                        }
                    })
                    .map_err(crate::exec::WorkRefusal::from)?;
                let pinned = inner.replica.read_snapshot();
                let (command, label) = match &request {
                    crate::exec::WorkRequest::Cancel { run, .. } => {
                        (crate::exec::Cmd::Cancel { run: *run }, "exec.work.cancel")
                    }
                    crate::exec::WorkRequest::Retry { .. } => (
                        crate::exec::Cmd::Try(continuation_try(
                            &pinned,
                            self.world.exec_specs(),
                            &ambient,
                            &request,
                        )?),
                        "exec.work.continue",
                    ),
                    crate::exec::WorkRequest::Resume { .. } => (
                        crate::exec::Cmd::Try(continuation_try(
                            &pinned,
                            self.world.exec_specs(),
                            &ambient,
                            &request,
                        )?),
                        "exec.work.resume",
                    ),
                    crate::exec::WorkRequest::Inspect { .. }
                    | crate::exec::WorkRequest::Watch { .. } => {
                        return Err(crate::exec::WorkRefusal::Unsupported(
                            "a read-only Work action cannot commit a lifecycle event",
                        ));
                    }
                };
                let now_millis = mechanics::wallclock::now_millis();
                let admission = OfferAdmission {
                    news: &inner.offers,
                    readies: &inner.readies,
                    now_millis,
                };
                let runtime = lower_exec(
                    std::slice::from_ref(&command),
                    self.world.exec_specs(),
                    &ambient,
                    operation,
                    0,
                    &pinned,
                    &admission,
                )
                .map_err(Failure::from)
                .map_err(crate::exec::WorkRefusal::from)?;
                let current = self
                    .authority
                    .resolve(&principal.device)
                    .ok_or(Rejection::Denied(DeniedCause::NotAMember))
                    .map_err(Failure::from)
                    .map_err(crate::exec::WorkRefusal::from)?;
                if current.authority_frontier != principal.authority_frontier {
                    return Err(crate::exec::WorkRefusal::Session(Failure::Conflict(
                        Conflict::AuthorityChanged,
                    )));
                }
                let demand = combine_exec_demands(&runtime.demands)
                    .map_err(Failure::from)
                    .map_err(crate::exec::WorkRefusal::from)?;
                let commit = replica::transaction::CommitContext {
                    space: &self.space,
                    signer: &self.identity,
                    authority_frontier: principal.authority_frontier.clone(),
                };
                let authorizer = SessionAuthorizer {
                    authority: self.authority.as_ref(),
                    space: &self.space,
                    world: &self.world_id,
                    actor: &principal.actor,
                    device: &principal.device,
                    authority_frontier: &principal.authority_frontier,
                    implementation_id: ambient.implementation,
                };
                let authorization = replica::transaction::CommitAuthorization {
                    actor: principal.actor.as_str(),
                    parent_manifest_root: pinned.root(),
                    demand,
                    intent_digest: digest,
                    authorizer: &authorizer,
                };
                let mut bodies = runtime.bodies;
                bodies.sort();
                bodies.dedup();
                let mut durable_change = crate::change::DurableChange::from_operations(
                    crate::change::Attribution {
                        operation,
                        actor: principal.actor.clone(),
                        device: principal.device.clone(),
                    },
                    &runtime.operations,
                );
                durable_change.cover_bodies(bodies.iter().cloned());
                let prior_publication = inner.world_publications.get(&self.world_id).cloned();
                let prior = inner.snapshot.clone();
                let candidate_materialization = inner.next_materialization;
                let fresh = {
                    let outcome = inner
                        .replica
                        .prepare_action(
                            &commit,
                            &authorization,
                            replica::receipt::Interpretation {
                                implementation_digest: self.implementation,
                                extractor_schema_digest: self.extractor_schema_digest.digest(),
                            },
                            &self.world_id,
                            &principal.device,
                            &operation,
                            &digest,
                            Vec::new(),
                            bodies,
                            label,
                            &runtime.operations,
                            &runtime.bindings,
                            &runtime.content_refs,
                        )
                        .map_err(commit_failure)
                        .map_err(crate::exec::WorkRefusal::from)?;
                    match outcome {
                        replica::transaction::PreparedActionOutcome::Replayed(_) => None,
                        replica::transaction::PreparedActionOutcome::Prepared(prepared) => {
                            let candidate_receipt = prepared.receipt().clone();
                            let snapshot = Arc::new(
                                prepared
                                    .candidate_snapshot(&prior)
                                    .map_err(commit_failure)
                                    .map_err(crate::exec::WorkRefusal::from)?,
                            );
                            durable_change.stabilize_prepared(&runtime.operations, &snapshot);
                            let id = crate::publication::WorldPublicationId::new(
                                crate::publication::PublicationId::new(
                                    snapshot.root(),
                                    self.implementation,
                                    self.extractor_schema_digest,
                                ),
                                candidate_materialization,
                            );
                            let publication = candidate_world_publication(
                                snapshot.clone(),
                                id,
                                prior_publication,
                                &self.world,
                                &self.world_id,
                                self.implementation,
                                self.extractor_schema_digest,
                                self.world.find_schemas(),
                                self.world.find_extractors(),
                                &candidate_receipt.bodies,
                            )
                            .map_err(|failure| {
                                crate::exec::WorkRefusal::Session(Failure::PersistenceCause {
                                    operation: "build candidate World publication",
                                    reason: format!("{failure:?}"),
                                })
                            })?;
                            let receipt = prepared
                                .finalize(&commit)
                                .map_err(commit_failure)
                                .map_err(crate::exec::WorkRefusal::from)?;
                            debug_assert_eq!(receipt, candidate_receipt);
                            Some((receipt, snapshot, publication))
                        }
                    }
                };
                if let Some((receipt, snapshot, publication)) = fresh {
                    let committed_publication = publication.id;
                    let parent = prior.root();
                    inner.publish_snapshot(snapshot, Some(parent), Some(&receipt.bodies));
                    debug_assert_eq!(inner.snapshot_materialization, candidate_materialization);
                    inner.install_world_publication(self.world_id.clone(), publication);
                    durable_change.cover_bodies(receipt.bodies.iter().cloned());
                    self.core.note_exec();
                    self.core.broadcaster.publish_change(
                        durable_change,
                        receipt.frontier,
                        false,
                        vec![AffectedWorldPublication {
                            world: self.world_id.clone(),
                            publication: committed_publication,
                        }],
                    );
                }
                consume_first_use_readies(
                    &mut inner,
                    std::slice::from_ref(&command),
                    &pinned,
                    &ambient.world,
                );
                work_reply(&inner.snapshot, &request)
            }
        }
    }

    /// Observe committed unresolved Runs and perform one local Attempt.
    ///
    /// Dispatch never precedes a durable event. A Started Run that this
    /// Station can handle is leased locally — citing live Offer news and a
    /// Ready when both exist, otherwise Station-only. A lease for this
    /// activation is begun; a Began this process just committed is invoked;
    /// an inherited Began or a prior-epoch Leased that never began is failed
    /// rather than retried.
    pub fn perform(
        &self,
        package: &crate::exec::Package,
        mut put_output: impl FnMut(&[u8]) -> Result<replica::content::ContentRef, Failure>,
    ) -> Result<crate::exec::PerformReport, Failure> {
        self.ensure_live()?;
        if !self.core.try_begin_perform() {
            return Ok(crate::exec::PerformReport::default());
        }
        struct Guard<'a>(&'a StationCore);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.end_perform();
            }
        }
        let _guard = Guard(&self.core);
        let mut report = crate::exec::PerformReport::default();
        for _ in 0..16 {
            let next = self.next_perform_action(package)?;
            match next {
                None => break,
                Some(PerformAction::Try(intent)) => {
                    let run = intent.run;
                    let attempt =
                        self.commit_perform_cmd(crate::exec::Cmd::Try(intent), "exec.perform.try")?;
                    if let Some(attempt) = attempt {
                        report
                            .steps
                            .push(crate::exec::PerformStep::Tried { run, attempt });
                    }
                }
                Some(PerformAction::Begin { run, attempt }) => {
                    self.commit_perform_event(
                        run,
                        crate::exec::RunEventKind::Began(crate::exec::Began {
                            run,
                            attempt,
                            executor: self.principal.actor.clone(),
                            device: self.principal.device.clone(),
                        }),
                        &[],
                        "exec.perform.began",
                    )?;
                    self.core.claim_attempt(attempt);
                    report
                        .steps
                        .push(crate::exec::PerformStep::Began { run, attempt });
                }
                Some(PerformAction::Invoke { run, attempt }) => {
                    match self.invoke_and_complete(package, run, attempt, &mut put_output) {
                        Ok(step) => {
                            self.core.release_attempt(attempt);
                            report.steps.push(step);
                        }
                        Err(failure) => {
                            let class = match failure {
                                Failure::Rejected(_) => crate::exec::FailureClass::Protocol,
                                _ => crate::exec::FailureClass::Backend,
                            };
                            let _ = self.commit_perform_event(
                                run,
                                crate::exec::RunEventKind::Failed(crate::exec::Failed {
                                    run,
                                    attempt,
                                    class,
                                    evidence: Vec::new(),
                                }),
                                &[],
                                "exec.perform.failed",
                            );
                            self.core.release_attempt(attempt);
                            report.steps.push(crate::exec::PerformStep::Failed {
                                run,
                                attempt,
                                class,
                            });
                            if matches!(
                                failure,
                                Failure::Interrupted
                                    | Failure::Persistence
                                    | Failure::PersistenceCause { .. }
                            ) {
                                return Err(failure);
                            }
                        }
                    }
                }
                Some(PerformAction::Recover { run, attempt }) => {
                    self.commit_perform_event(
                        run,
                        crate::exec::RunEventKind::Failed(crate::exec::Failed {
                            run,
                            attempt,
                            class: crate::exec::FailureClass::Unknown,
                            evidence: Vec::new(),
                        }),
                        &[],
                        "exec.perform.unknown",
                    )?;
                    self.core.release_attempt(attempt);
                    report.steps.push(crate::exec::PerformStep::Failed {
                        run,
                        attempt,
                        class: crate::exec::FailureClass::Unknown,
                    });
                }
            }
        }
        Ok(report)
    }

    fn next_perform_action(
        &self,
        package: &crate::exec::Package,
    ) -> Result<Option<PerformAction>, Failure> {
        let inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        let unresolved =
            crate::exec::scan_unresolved(&inner.snapshot, &self.world_id).map_err(exec_invalid)?;
        drop(inner);
        for item in unresolved {
            if let Some(attempt) = item.run.attempts.iter().rev().find(|attempt| {
                attempt.station == self.principal.station
                    && attempt.station_epoch != self.epoch
                    && attempt.outcomes.is_empty()
                    && attempt.failures.is_empty()
                    && attempt.cancellations.is_empty()
            }) {
                return Ok(Some(PerformAction::Recover {
                    run: item.run.id,
                    attempt: attempt.id,
                }));
            }
            if let Some(attempt) = item.run.attempts.iter().rev().find(|attempt| {
                attempt.station == self.principal.station
                    && attempt.station_epoch == self.epoch
                    && attempt.outcomes.is_empty()
                    && attempt.failures.is_empty()
                    && attempt.cancellations.is_empty()
            }) {
                let began = matches!(
                    attempt.began.as_slice(),
                    [began] if began.predecessors.contains(&attempt.leased_event)
                );
                if !began {
                    return Ok(Some(PerformAction::Begin {
                        run: item.run.id,
                        attempt: attempt.id,
                    }));
                }
                if self.core.is_inflight(attempt.id) {
                    return Ok(Some(PerformAction::Invoke {
                        run: item.run.id,
                        attempt: attempt.id,
                    }));
                }
                return Ok(Some(PerformAction::Recover {
                    run: item.run.id,
                    attempt: attempt.id,
                }));
            }
            if self.can_local_try(package, &item.run) {
                let intent = self.local_try_intent(&item.run)?;
                return Ok(Some(PerformAction::Try(intent)));
            }
        }
        Ok(None)
    }

    fn can_local_try(&self, package: &crate::exec::Package, run: &crate::exec::Run) -> bool {
        if !run.is_unresolved() || !run.cancel_asked.is_empty() {
            return false;
        }
        let Ok(attempt_count) = u32::try_from(run.attempts.len()) else {
            return false;
        };
        if attempt_count >= run.started.limits.attempts {
            return false;
        }
        let handles = package
            .builds()
            .iter()
            .any(|build| build.id == run.started.build)
            && package.handlers().iter().any(|handler| {
                handler.binding().build == run.started.build
                    && handler.binding().spec == run.started.spec
            });
        if !handles {
            return false;
        }
        if run.attempts.is_empty() {
            return true;
        }
        let spec = self.world.exec_specs().iter().find(|spec| {
            spec.name == run.started.spec.name && spec.version == run.started.spec.version
        });
        let Some(spec) = spec else {
            return false;
        };
        if !matches!(spec.resume, crate::exec::Resume::Restart) {
            return false;
        }
        // Attempt ids are content hashes, so list order is not time. A later
        // Return or handler failure is not another outbox retry.
        run.attempts
            .iter()
            .max_by_key(|attempt| (attempt.fence, attempt.leased_event))
            .is_some_and(|attempt| {
                attempt.station == self.principal.station
                    && attempt.outcomes.is_empty()
                    && attempt.cancellations.is_empty()
                    && attempt
                        .failures
                        .iter()
                        .any(|fact| fact.value.class == crate::exec::FailureClass::Unknown)
            })
    }

    fn local_try_intent(&self, run: &crate::exec::Run) -> Result<crate::exec::Try, Failure> {
        let mut intent =
            crate::exec::Try::local_first(run, self.principal.station.clone(), self.epoch)
                .map_err(exec_invalid)?;
        let now = mechanics::wallclock::now_millis();
        let inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        let cited = inner.offers.values().find_map(|held| {
            if held.usable_at(now).is_err()
                || !held.covers(run.started.build)
                || held.space != self.space
                || held.station != self.principal.station
                || held.station_epoch != self.epoch
                || held.world != self.world_id
                || held.world_build != self.implementation
            {
                return None;
            }
            let ready = inner.readies.get(&held.id)?;
            if ready.challenge.usable_at(now).is_err()
                || ready.ready.validate_for(&ready.challenge).is_err()
                || ready.challenge.station != self.principal.station
                || ready.challenge.station_epoch != self.epoch
            {
                return None;
            }
            Some(held.reference())
        });
        drop(inner);
        if let Some(offer) = cited {
            intent = intent.with_offer(offer).map_err(exec_invalid)?;
        }
        Ok(intent)
    }

    fn invoke_and_complete(
        &self,
        package: &crate::exec::Package,
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
        put_output: &mut impl FnMut(&[u8]) -> Result<replica::content::ContentRef, Failure>,
    ) -> Result<crate::exec::PerformStep, Failure> {
        let snapshot = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            inner.replica.read_snapshot()
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let dispatcher = crate::exec::Dispatcher::new(package, crate::exec::InProcess::new());
        let mut completion = dispatcher
            .invoke(&snapshot, &self.world_id, run, attempt, &cancel)
            .map_err(|failure| match failure {
                crate::exec::DispatchFailure::Backend(crate::exec::Failure::Cancelled) => {
                    Failure::Rejected(Rejection::InvalidRequest)
                }
                crate::exec::DispatchFailure::Backend(crate::exec::Failure::Handler) => {
                    Failure::CallbackPanicked
                }
                _ => Failure::Rejected(Rejection::ContractViolation),
            })?;
        if !completion.output_blobs().is_empty() {
            let mut refs = Vec::new();
            let mut bytes = 0u64;
            for blob in completion.output_blobs() {
                let content = put_output(blob)?;
                let added = u64::try_from(blob.len())
                    .map_err(|_| Failure::Rejected(Rejection::LimitExceeded))?;
                bytes = bytes
                    .checked_add(added)
                    .ok_or(Failure::Rejected(Rejection::LimitExceeded))?;
                refs.push(content);
            }
            refs.sort_unstable();
            refs.dedup();
            completion
                .bind_output_content(refs, bytes)
                .map_err(|_| Failure::Rejected(Rejection::ContractViolation))?;
        }
        let (run_state, _, _) = crate::exec::read_committed_run(&snapshot, &self.world_id, run)
            .map_err(exec_invalid)?
            .ok_or(Rejection::ContractViolation)?;
        let attempt_state = run_state
            .attempts
            .iter()
            .find(|candidate| candidate.id == attempt)
            .ok_or(Rejection::ContractViolation)?;
        let [began] = attempt_state.began.as_slice() else {
            return Err(Failure::Rejected(Rejection::ContractViolation));
        };
        let events = completion
            .events(run, attempt, vec![began.event])
            .map_err(exec_invalid)?;
        for event in events {
            let content = match &event.kind {
                crate::exec::RunEventKind::Returned(returned) => returned.output_content.clone(),
                crate::exec::RunEventKind::Saved(saved) => vec![saved.checkpoint.content],
                _ => Vec::new(),
            };
            self.commit_perform_event(run, event.kind, &content, "exec.perform.complete")?;
        }
        for child in completion.children() {
            self.commit_perform_cmd(crate::exec::Cmd::Start(child.clone()), "exec.perform.child")?;
        }
        Ok(crate::exec::PerformStep::Returned { run, attempt })
    }

    fn commit_perform_cmd(
        &self,
        command: crate::exec::Cmd,
        label: &'static str,
    ) -> Result<Option<crate::exec::AttemptId>, Failure> {
        let operation = crate::action::RequestId::mint().as_bytes();
        let digest = command.digest().map_err(exec_invalid)?;
        let mut inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        let principal = self.fresh_principal().map_err(Failure::from)?;
        let ambient = self
            .ambient(&principal, inner.snapshot.root())
            .map_err(ambient_failure)?;
        let pinned = inner.replica.read_snapshot();
        let now_millis = mechanics::wallclock::now_millis();
        let admission = OfferAdmission {
            news: &inner.offers,
            readies: &inner.readies,
            now_millis,
        };
        let runtime = lower_exec(
            std::slice::from_ref(&command),
            self.world.exec_specs(),
            &ambient,
            operation,
            0,
            &pinned,
            &admission,
        )?;
        let attempt = match &command {
            crate::exec::Cmd::Try(intent) => Some(crate::exec::derive_attempt_id(
                intent.run,
                &ambient.principal.device,
                operation,
                0,
            )),
            _ => None,
        };
        self.commit_runtime_effect(
            &mut inner, &principal, &ambient, operation, digest, label, runtime,
        )?;
        consume_first_use_readies(
            &mut inner,
            std::slice::from_ref(&command),
            &pinned,
            &ambient.world,
        );
        Ok(attempt)
    }

    fn commit_perform_event(
        &self,
        run: crate::exec::RunId,
        kind: crate::exec::RunEventKind,
        content: &[replica::content::ContentRef],
        label: &'static str,
    ) -> Result<(), Failure> {
        let operation = crate::action::RequestId::mint().as_bytes();
        let digest = kind_digest(&kind);
        let mut inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        let principal = self.fresh_principal().map_err(Failure::from)?;
        let ambient = self
            .ambient(&principal, inner.snapshot.root())
            .map_err(ambient_failure)?;
        let pinned = inner.replica.read_snapshot();
        let runtime = lower_lifecycle_event(
            &pinned,
            self.world.exec_specs(),
            &ambient,
            run,
            kind,
            content,
        )?;
        self.commit_runtime_effect(
            &mut inner, &principal, &ambient, operation, digest, label, runtime,
        )
    }

    fn commit_runtime_effect(
        &self,
        inner: &mut CoreInner,
        principal: &PrincipalFacts,
        ambient: &Ambient,
        operation: [u8; 16],
        digest: [u8; 32],
        label: &'static str,
        runtime: RuntimeEffect,
    ) -> Result<(), Failure> {
        let current = self
            .authority
            .resolve(&principal.device)
            .ok_or(Rejection::Denied(DeniedCause::NotAMember))?;
        if current.authority_frontier != principal.authority_frontier {
            return Err(Failure::Conflict(Conflict::AuthorityChanged));
        }
        let demand = combine_exec_demands(&runtime.demands)?;
        let commit = replica::transaction::CommitContext {
            space: &self.space,
            signer: &self.identity,
            authority_frontier: principal.authority_frontier.clone(),
        };
        let authorizer = SessionAuthorizer {
            authority: self.authority.as_ref(),
            space: &self.space,
            world: &self.world_id,
            actor: &principal.actor,
            device: &principal.device,
            authority_frontier: &principal.authority_frontier,
            implementation_id: ambient.implementation,
        };
        let authorization = replica::transaction::CommitAuthorization {
            actor: principal.actor.as_str(),
            parent_manifest_root: inner.snapshot.root(),
            demand,
            intent_digest: digest,
            authorizer: &authorizer,
        };
        let mut bodies = runtime.bodies.clone();
        bodies.sort();
        bodies.dedup();
        let mut durable_change = crate::change::DurableChange::from_operations(
            crate::change::Attribution {
                operation,
                actor: principal.actor.clone(),
                device: principal.device.clone(),
            },
            &runtime.operations,
        );
        durable_change.cover_bodies(bodies.iter().cloned());
        let prior_publication = inner.world_publications.get(&self.world_id).cloned();
        let prior = inner.snapshot.clone();
        let candidate_materialization = inner.next_materialization;
        let (receipt, fresh) = {
            let outcome = inner
                .replica
                .prepare_action(
                    &commit,
                    &authorization,
                    replica::receipt::Interpretation {
                        implementation_digest: self.implementation,
                        extractor_schema_digest: self.extractor_schema_digest.digest(),
                    },
                    &self.world_id,
                    &principal.device,
                    &operation,
                    &digest,
                    Vec::new(),
                    bodies,
                    label,
                    &runtime.operations,
                    &runtime.bindings,
                    &runtime.content_refs,
                )
                .map_err(commit_failure)?;
            match outcome {
                replica::transaction::PreparedActionOutcome::Replayed(receipt) => (receipt, None),
                replica::transaction::PreparedActionOutcome::Prepared(prepared) => {
                    let candidate_receipt = prepared.receipt().clone();
                    let snapshot = Arc::new(
                        prepared
                            .candidate_snapshot(&prior)
                            .map_err(commit_failure)?,
                    );
                    durable_change.stabilize_prepared(&runtime.operations, &snapshot);
                    let id = crate::publication::WorldPublicationId::new(
                        crate::publication::PublicationId::new(
                            snapshot.root(),
                            self.implementation,
                            self.extractor_schema_digest,
                        ),
                        candidate_materialization,
                    );
                    let publication = candidate_world_publication(
                        snapshot.clone(),
                        id,
                        prior_publication,
                        &self.world,
                        &self.world_id,
                        self.implementation,
                        self.extractor_schema_digest,
                        self.world.find_schemas(),
                        self.world.find_extractors(),
                        &candidate_receipt.bodies,
                    )
                    .map_err(|failure| Failure::PersistenceCause {
                        operation: "build candidate World publication",
                        reason: format!("{failure:?}"),
                    })?;
                    let receipt = prepared.finalize(&commit).map_err(commit_failure)?;
                    debug_assert_eq!(receipt, candidate_receipt);
                    (receipt, Some((snapshot, publication)))
                }
            }
        };
        if let Some((snapshot, publication)) = fresh {
            let committed_publication = publication.id;
            let parent = prior.root();
            inner.publish_snapshot(snapshot, Some(parent), Some(&receipt.bodies));
            inner.install_world_publication(self.world_id.clone(), publication);
            durable_change.cover_bodies(receipt.bodies.iter().cloned());
            self.core.note_exec();
            self.core.broadcaster.publish_change(
                durable_change,
                receipt.frontier,
                false,
                vec![AffectedWorldPublication {
                    world: self.world_id.clone(),
                    publication: committed_publication,
                }],
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn test_lease(
        &self,
        package: &crate::exec::Package,
    ) -> Result<(crate::exec::RunId, crate::exec::AttemptId), Failure> {
        match self.next_perform_action(package)? {
            Some(PerformAction::Try(intent)) => {
                let run = intent.run;
                let attempt = self
                    .commit_perform_cmd(crate::exec::Cmd::Try(intent), "exec.test.try")?
                    .ok_or(Rejection::ContractViolation)?;
                Ok((run, attempt))
            }
            _ => Err(Failure::Rejected(Rejection::ContractViolation)),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_lease_and_begin(
        &self,
        package: &crate::exec::Package,
    ) -> Result<(crate::exec::RunId, crate::exec::AttemptId), Failure> {
        let (run, attempt) = self.test_lease(package)?;
        self.commit_perform_event(
            run,
            crate::exec::RunEventKind::Began(crate::exec::Began {
                run,
                attempt,
                executor: self.principal.actor.clone(),
                device: self.principal.device.clone(),
            }),
            &[],
            "exec.test.began",
        )?;
        Ok((run, attempt))
    }

    /// Admit one generic bounded Find request against a pinned read generation.
    ///
    /// The caller supplies only semantic Query intent. Runtime derives the
    /// Station epoch, Space, World, active implementation, fresh principal,
    /// authority frontier, retained Manifest root, and local policy while the
    /// writer is held, then releases it before entering the common bounded,
    /// gate-first evaluator used by World projection contexts as well.
    pub fn find(
        &self,
        query: crate::find::Query,
    ) -> Result<crate::find::Answer, crate::find::Failure> {
        struct BuildPlan {
            flight: Arc<PublicationFlight>,
            key: (WorldId, crate::publication::PublicationId),
            root: [u8; 32],
            reserved_materialization: crate::publication::MaterializationId,
            snapshot: Option<(
                Arc<replica::ReadSnapshot>,
                crate::publication::MaterializationId,
            )>,
            reader: Option<replica::GenerationReader>,
            world: Arc<dyn World>,
            schemas: Vec<crate::find::Schema>,
            extractors: Vec<crate::find::Extractor>,
            install_current: bool,
        }

        enum PublicationPlan {
            Ready(Arc<WorldPublication>),
            Follow(Arc<PublicationFlight>),
            Build(BuildPlan),
        }

        if !self.alive.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::find::Failure::Interrupted);
        }
        let query_digest = query.digest()?;
        if !self.find_policy.bound.contains(query.bound) {
            return Err(crate::find::Failure::PolicyExceeded);
        }

        let requested_publication = query.publication;
        let (plan, gates, ambient) = {
            let mut inner = self.core.lock();
            if inner.closed {
                return Err(crate::find::Failure::Interrupted);
            }
            let principal = self
                .fresh_principal()
                .map_err(|_| crate::find::Failure::PrincipalDenied)?;
            let mut ambient = self.ambient(&principal, inner.snapshot.root()).map_err(
                |failure| match failure {
                    AmbientFailure::NoActiveImplementation => {
                        crate::find::Failure::NoActiveImplementation
                    }
                    AmbientFailure::ImplementationUnavailable => {
                        crate::find::Failure::ImplementationUnavailable
                    }
                    AmbientFailure::AuthorityUnavailable(detail) => {
                        crate::find::Failure::AuthorityUnavailable(detail)
                    }
                },
            )?;
            let routing_expected = crate::find::Coordinates {
                epoch: ambient.epoch,
                space: ambient.space.clone(),
                world: ambient.world.clone(),
                implementation: ambient.implementation,
                root: inner.snapshot.root(),
                extractor_schema_digest: ambient.extractor_schema_digest,
                materialization: inner.snapshot_materialization,
                actor: principal.actor.clone(),
                device: principal.device.clone(),
                authority_frontier: principal.authority_frontier.clone(),
                query: query_digest,
                schema: query.schema.clone(),
            };
            let cursor_coordinates = query
                .cursor
                .as_ref()
                .map(|cursor| cursor.route_for(&routing_expected, &query))
                .transpose()?;
            let cursor_publication = cursor_coordinates
                .as_ref()
                .map(crate::find::Coordinates::publication);
            let semantic = match (requested_publication, cursor_publication) {
                (Some(requested), Some(cursor)) if requested != cursor => {
                    return Err(crate::find::Invalid::CursorMismatch("publication").into())
                }
                (Some(requested), _) => requested,
                (None, Some(cursor)) => cursor,
                (None, None) => crate::publication::PublicationId::new(
                    inner.snapshot.root(),
                    ambient.implementation,
                    ambient.extractor_schema_digest,
                ),
            };

            // A historical root is interpreted only by the exact installed
            // package named in its semantic PublicationId. The active
            // implementation still supplies current authority and principal
            // facts, but it does not get to reinterpret old material.
            let active_contract = semantic.implementation_digest == ambient.implementation
                && semantic.extractor_schema_digest == ambient.extractor_schema_digest;
            let (find_world, find_schemas, find_extractors, extractor_schema_digest) =
                if let (Some(world), Some(descriptor)) = (
                    self.registry
                        .world_for(&self.world_id, semantic.implementation_digest),
                    self.registry
                        .descriptor_for(&self.world_id, semantic.implementation_digest),
                ) {
                    let digest = crate::publication::ExtractorSchemaDigest::derive(
                        &descriptor.find_schemas,
                        &descriptor.find_extractors,
                    )
                    .map_err(|_| crate::find::Failure::ImplementationUnavailable)?;
                    (
                        world,
                        descriptor.find_schemas.clone(),
                        descriptor.find_extractors.clone(),
                        digest,
                    )
                } else if active_contract && semantic.implementation_digest == self.implementation {
                    // Explicit low-level embedding mode. Its unreviewed World
                    // may serve only the current Session contract; it cannot
                    // resolve a different or historical implementation id.
                    (
                        self.world.clone(),
                        self.world.find_schemas().to_vec(),
                        self.world.find_extractors().to_vec(),
                        self.extractor_schema_digest,
                    )
                } else {
                    return Err(crate::find::Failure::ImplementationUnavailable);
                };
            if semantic.extractor_schema_digest != extractor_schema_digest {
                return Err(crate::find::Failure::ImplementationUnavailable);
            }
            let declaration = find_schemas
                .iter()
                .find(|schema| schema.reference == query.schema)
                .ok_or(crate::find::Invalid::UndeclaredSchema("query schema"))?;
            query.validate_within_schema(declaration)?;

            let mut granted = Vec::new();
            for gate in &declaration.gates {
                match self.authority.evaluate_read(
                    &principal.actor,
                    &principal.authority_frontier,
                    &gate.demand,
                ) {
                    Ok(true) => granted.push(gate.reference.clone()),
                    Ok(false) => {}
                    Err(detail) => {
                        return Err(crate::find::Failure::AuthorityUnavailable(detail));
                    }
                }
            }
            let gates = crate::find_evaluator::GrantedGates::new(granted);

            let plan = if let Some(cursor) = cursor_coordinates {
                let id = cursor.world_publication();
                let key = (self.world_id.clone(), id);
                inner.purge_cursor_leases();
                let publication = inner
                    .world_publications
                    .get(&self.world_id)
                    .filter(|publication| publication.id == id)
                    .cloned()
                    .or_else(|| {
                        inner
                            .retained_world_publications
                            .get(&(self.world_id.clone(), id))
                            .cloned()
                    })
                    .or_else(|| inner.leased_world_publication(&key))
                    .ok_or(crate::find::Failure::PublicationExpired)?;
                PublicationPlan::Ready(publication)
            } else {
                if let Some(publication) = inner
                    .world_publications
                    .get(&self.world_id)
                    .filter(|publication| publication.id.publication == semantic)
                    .cloned()
                    .or_else(|| {
                        inner
                            .retained_world_publications
                            .iter()
                            .find(|((world, id), _)| {
                                world == &self.world_id && id.publication == semantic
                            })
                            .map(|(_, publication)| publication.clone())
                    })
                {
                    PublicationPlan::Ready(publication)
                } else {
                    let key = (self.world_id.clone(), semantic);
                    if let Some(flight) = inner.publication_flights.get(&key).cloned() {
                        PublicationPlan::Follow(flight)
                    } else {
                        let flight = Arc::new(PublicationFlight::new());
                        inner
                            .publication_flights
                            .insert(key.clone(), flight.clone());
                        let root = semantic.manifest_root;
                        let snapshot = if root == inner.snapshot.root() {
                            Some((inner.snapshot.clone(), inner.snapshot_materialization))
                        } else {
                            inner.generations.get(&root).map(|generation| {
                                (generation.snapshot.clone(), generation.materialization)
                            })
                        };
                        let reader = snapshot
                            .is_none()
                            .then(|| inner.replica.generation_reader(inner.snapshot.clone()));
                        let reserved_materialization = if let Some((_, materialization)) = snapshot
                        {
                            materialization
                        } else {
                            inner.reserve_materialization()
                        };
                        let install_current = active_contract && root == inner.snapshot.root();
                        PublicationPlan::Build(BuildPlan {
                            flight,
                            key,
                            root,
                            reserved_materialization,
                            snapshot,
                            reader,
                            world: find_world,
                            schemas: find_schemas,
                            extractors: find_extractors,
                            install_current,
                        })
                    }
                }
            };
            ambient.root = semantic.manifest_root;
            ambient.implementation = semantic.implementation_digest;
            ambient.extractor_schema_digest = extractor_schema_digest;
            (plan, gates, ambient)
        };

        // Cold durable generation reconstruction and World extraction happen
        // on immutable handles after releasing the Station writer. Identical
        // requests share one flight; followers wait here, never on the writer.
        let publication = match plan {
            PublicationPlan::Ready(publication) => publication,
            PublicationPlan::Follow(flight) => flight.wait()?,
            PublicationPlan::Build(plan) => {
                let finish = |result: Result<Arc<WorldPublication>, crate::find::Failure>| {
                    let mut inner = self.core.lock();
                    if inner
                        .publication_flights
                        .get(&plan.key)
                        .is_some_and(|current| Arc::ptr_eq(current, &plan.flight))
                    {
                        inner.publication_flights.remove(&plan.key);
                    }
                    drop(inner);
                    plan.flight.complete(result.clone());
                    result
                };

                let (snapshot, materialization) = if let Some(snapshot) = plan.snapshot {
                    snapshot
                } else {
                    let Some(reader) = plan.reader.as_ref() else {
                        let failure = crate::find::Failure::PublicationUnavailable;
                        let _ = finish(Err(failure.clone()));
                        return Err(failure);
                    };
                    let reconstructed = match reader.read_generation(&plan.root) {
                        Ok(Some(snapshot)) => Arc::new(snapshot),
                        Ok(None) | Err(_) => {
                            let failure = crate::find::Failure::PublicationUnavailable;
                            let _ = finish(Err(failure.clone()));
                            return Err(failure);
                        }
                    };
                    let mut inner = self.core.lock();
                    if inner.closed {
                        drop(inner);
                        let failure = crate::find::Failure::Interrupted;
                        let _ = finish(Err(failure.clone()));
                        return Err(failure);
                    }
                    if let Some(cached) = inner.generations.get(&plan.root).cloned() {
                        (cached.snapshot, cached.materialization)
                    } else {
                        inner.cache_generation_at(
                            plan.root,
                            reconstructed.clone(),
                            None,
                            plan.reserved_materialization,
                        );
                        (reconstructed, plan.reserved_materialization)
                    }
                };
                let id = crate::publication::WorldPublicationId::new(plan.key.1, materialization);
                let corpus = match build_world_corpus(
                    &snapshot,
                    &plan.world,
                    &self.world_id,
                    id,
                    &plan.schemas,
                    &plan.extractors,
                ) {
                    Ok(corpus) => Arc::new(corpus),
                    Err(_) => {
                        let failure = crate::find::Failure::Unavailable;
                        let _ = finish(Err(failure.clone()));
                        return Err(failure);
                    }
                };
                let publication = Arc::new(WorldPublication {
                    id,
                    snapshot,
                    corpus,
                });
                {
                    let mut inner = self.core.lock();
                    if inner.closed {
                        drop(inner);
                        let failure = crate::find::Failure::Interrupted;
                        let _ = finish(Err(failure.clone()));
                        return Err(failure);
                    }
                    if plan.install_current
                        && inner.snapshot.root() == plan.root
                        && inner.snapshot_materialization == materialization
                    {
                        inner.install_world_publication(self.world_id.clone(), publication.clone());
                    } else {
                        inner.retain_world_publication(self.world_id.clone(), publication.clone());
                    }
                }
                finish(Ok(publication))?
            }
        };

        let coordinates = crate::find::Coordinates {
            epoch: ambient.epoch,
            space: ambient.space,
            world: ambient.world,
            implementation: ambient.implementation,
            root: publication.id.publication.manifest_root,
            extractor_schema_digest: ambient.extractor_schema_digest,
            materialization: publication.id.materialization,
            actor: ambient.principal.actor,
            device: ambient.principal.device,
            authority_frontier: ambient.principal.authority_frontier,
            query: query_digest,
            schema: query.schema.clone(),
        };
        let answer = crate::find::evaluate(crate::find::Admission {
            query,
            coordinates,
            policy: ambient.find_policy,
            snapshot: publication.snapshot.clone(),
            corpus: publication.corpus.clone(),
            gates,
        })?;
        if answer.next_cursor().is_some() {
            let mut inner = self.core.lock();
            if inner.closed {
                return Err(crate::find::Failure::Interrupted);
            }
            inner
                .lease_world_publication(self.world_id.clone(), publication)
                .map_err(|_| crate::find::Failure::CursorCapacityExceeded)?;
        }
        Ok(answer)
    }

    #[cfg(test)]
    pub(crate) fn expire_cursor_leases_for_test(&self) {
        let mut inner = self.core.lock();
        let expired = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        for lease in inner.cursor_leases.values_mut() {
            lease.expires = expired;
        }
        inner.purge_cursor_leases();
    }

    /// Query one exact World publication through the same resolution path used
    /// by Find. `query.publication == None` selects the authority-active current
    /// package and root. An explicit PublicationId resolves its exact installed
    /// implementation/extractor contract; Runtime never interprets an old root
    /// through ambient code. Cold generation reconstruction and extraction run
    /// outside the Station writer and are single-flighted with Find.
    pub fn query(&self, query: Query) -> Result<Projection, Failure> {
        struct BuildPlan {
            flight: Arc<PublicationFlight>,
            key: (WorldId, crate::publication::PublicationId),
            root: [u8; 32],
            reserved_materialization: crate::publication::MaterializationId,
            snapshot: Option<(
                Arc<replica::ReadSnapshot>,
                crate::publication::MaterializationId,
            )>,
            reader: Option<replica::GenerationReader>,
            world: Arc<dyn World>,
            find_schemas: Vec<crate::find::Schema>,
            find_extractors: Vec<crate::find::Extractor>,
            install_current: bool,
        }

        enum Plan {
            Ready(Arc<WorldPublication>),
            Follow(Arc<PublicationFlight>),
            Build(BuildPlan),
        }

        self.ensure_live()?;
        self.ensure_within_limit(query.payload.len())?;
        let requested = query.publication;
        let principal = self.fresh_principal()?;
        let (plan, query_world, world_schemas, find_schemas, find_extractors) = {
            let mut inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            let ambient = self
                .ambient(&principal, inner.snapshot.root())
                .map_err(|failure| match failure {
                    AmbientFailure::NoActiveImplementation => {
                        Failure::Rejected(Rejection::NoActiveImplementation)
                    }
                    AmbientFailure::ImplementationUnavailable => {
                        Failure::Rejected(Rejection::ImplementationUnavailable)
                    }
                    AmbientFailure::AuthorityUnavailable(detail) => {
                        Failure::AuthorityUnavailable(detail)
                    }
                })?;
            let semantic = requested.unwrap_or_else(|| {
                crate::publication::PublicationId::new(
                    inner.snapshot.root(),
                    ambient.implementation,
                    ambient.extractor_schema_digest,
                )
            });
            let active_contract = semantic.implementation_digest == ambient.implementation
                && semantic.extractor_schema_digest == ambient.extractor_schema_digest;
            let (query_world, find_schemas, find_extractors, extractor_schema_digest) =
                if let (Some(world), Some(descriptor)) = (
                    self.registry
                        .world_for(&self.world_id, semantic.implementation_digest),
                    self.registry
                        .descriptor_for(&self.world_id, semantic.implementation_digest),
                ) {
                    let digest = crate::publication::ExtractorSchemaDigest::derive(
                        &descriptor.find_schemas,
                        &descriptor.find_extractors,
                    )
                    .map_err(|_| Rejection::ImplementationUnavailable)?;
                    (
                        world,
                        descriptor.find_schemas.clone(),
                        descriptor.find_extractors.clone(),
                        digest,
                    )
                } else if active_contract && semantic.implementation_digest == self.implementation {
                    (
                        self.world.clone(),
                        self.world.find_schemas().to_vec(),
                        self.world.find_extractors().to_vec(),
                        self.extractor_schema_digest,
                    )
                } else {
                    return Err(Rejection::ImplementationUnavailable.into());
                };
            if semantic.extractor_schema_digest != extractor_schema_digest {
                return Err(Rejection::ImplementationUnavailable.into());
            }
            let world_schemas = query_world.schemas().to_vec();
            Self::ensure_readable_schema_in(&world_schemas, &query.schema, query.schema_version)?;

            let plan = if let Some(publication) = inner
                .world_publications
                .get(&self.world_id)
                .filter(|publication| publication.id.publication == semantic)
                .cloned()
                .or_else(|| {
                    inner
                        .retained_world_publications
                        .iter()
                        .find(|((world, id), _)| {
                            world == &self.world_id && id.publication == semantic
                        })
                        .map(|(_, publication)| publication.clone())
                }) {
                Plan::Ready(publication)
            } else {
                let key = (self.world_id.clone(), semantic);
                if let Some(flight) = inner.publication_flights.get(&key).cloned() {
                    Plan::Follow(flight)
                } else {
                    let flight = Arc::new(PublicationFlight::new());
                    inner
                        .publication_flights
                        .insert(key.clone(), flight.clone());
                    let root = semantic.manifest_root;
                    let snapshot = if root == inner.snapshot.root() {
                        Some((inner.snapshot.clone(), inner.snapshot_materialization))
                    } else {
                        inner.generations.get(&root).map(|generation| {
                            (generation.snapshot.clone(), generation.materialization)
                        })
                    };
                    let reader = snapshot
                        .is_none()
                        .then(|| inner.replica.generation_reader(inner.snapshot.clone()));
                    let reserved_materialization = if let Some((_, materialization)) = snapshot {
                        materialization
                    } else {
                        inner.reserve_materialization()
                    };
                    Plan::Build(BuildPlan {
                        flight,
                        key,
                        root,
                        reserved_materialization,
                        snapshot,
                        reader,
                        world: query_world.clone(),
                        find_schemas: find_schemas.clone(),
                        find_extractors: find_extractors.clone(),
                        install_current: active_contract && root == inner.snapshot.root(),
                    })
                }
            };
            (
                plan,
                query_world,
                world_schemas,
                find_schemas,
                find_extractors,
            )
        };

        let publication = match plan {
            Plan::Ready(publication) => publication,
            Plan::Follow(flight) => flight.wait().map_err(query_publication_failure)?,
            Plan::Build(plan) => {
                let finish = |result: Result<Arc<WorldPublication>, crate::find::Failure>| {
                    let mut inner = self.core.lock();
                    if inner
                        .publication_flights
                        .get(&plan.key)
                        .is_some_and(|current| Arc::ptr_eq(current, &plan.flight))
                    {
                        inner.publication_flights.remove(&plan.key);
                    }
                    drop(inner);
                    plan.flight.complete(result.clone());
                    result
                };
                let (snapshot, materialization) = if let Some(snapshot) = plan.snapshot {
                    snapshot
                } else {
                    let Some(reader) = plan.reader.as_ref() else {
                        let failure = crate::find::Failure::PublicationUnavailable;
                        let _ = finish(Err(failure));
                        return Err(Failure::GenerationUnavailable);
                    };
                    let reconstructed = match reader.read_generation(&plan.root) {
                        Ok(Some(snapshot)) => Arc::new(snapshot),
                        Ok(None) | Err(_) => {
                            let failure = crate::find::Failure::PublicationUnavailable;
                            let _ = finish(Err(failure));
                            return Err(Failure::GenerationUnavailable);
                        }
                    };
                    let mut inner = self.core.lock();
                    if inner.closed {
                        drop(inner);
                        let failure = crate::find::Failure::Interrupted;
                        let _ = finish(Err(failure));
                        return Err(Failure::Interrupted);
                    }
                    if let Some(cached) = inner.generations.get(&plan.root).cloned() {
                        (cached.snapshot, cached.materialization)
                    } else {
                        inner.cache_generation_at(
                            plan.root,
                            reconstructed.clone(),
                            None,
                            plan.reserved_materialization,
                        );
                        (reconstructed, plan.reserved_materialization)
                    }
                };
                let id = crate::publication::WorldPublicationId::new(plan.key.1, materialization);
                let corpus = match build_world_corpus(
                    &snapshot,
                    &plan.world,
                    &self.world_id,
                    id,
                    &plan.find_schemas,
                    &plan.find_extractors,
                ) {
                    Ok(corpus) => Arc::new(corpus),
                    Err(_) => {
                        let failure = crate::find::Failure::Unavailable;
                        let _ = finish(Err(failure));
                        return Err(Failure::PersistenceCause {
                            operation: "prepare exact World query publication",
                            reason: "extractor unavailable".into(),
                        });
                    }
                };
                let publication = Arc::new(WorldPublication {
                    id,
                    snapshot,
                    corpus,
                });
                {
                    let mut inner = self.core.lock();
                    if inner.closed {
                        drop(inner);
                        let failure = crate::find::Failure::Interrupted;
                        let _ = finish(Err(failure));
                        return Err(Failure::Interrupted);
                    }
                    if plan.install_current
                        && inner.snapshot.root() == plan.root
                        && inner.snapshot_materialization == materialization
                    {
                        inner.install_world_publication(self.world_id.clone(), publication.clone());
                    } else {
                        inner.retain_world_publication(self.world_id.clone(), publication.clone());
                    }
                }
                finish(Ok(publication.clone())).map_err(query_publication_failure)?
            }
        };
        self.query_publication(
            query,
            principal,
            publication,
            query_world,
            world_schemas,
            find_schemas,
            find_extractors,
        )
    }

    /// The current generation coordinate.
    pub fn snapshot_id(&self) -> Result<WorldSnapshotId, Failure> {
        self.ensure_live()?;
        let inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        Ok(WorldSnapshotId::new(
            self.world_id.clone(),
            inner.snapshot.root(),
        ))
    }

    /// Retained ancestry, oldest first. A converged branch may later add more
    /// than one parent; the public coordinate does not encode local sequence.
    pub fn generations(&self) -> Result<Vec<WorldGeneration>, Failure> {
        self.ensure_live()?;
        let inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        let durable = inner
            .replica
            .read_generations()
            .map_err(|_| Failure::Persistence)?;
        let mut rows: Vec<WorldGeneration> = durable
            .into_iter()
            .map(|generation| WorldGeneration {
                id: WorldSnapshotId::new(self.world_id.clone(), generation.root),
                parent: generation
                    .parent
                    .map(|parent| WorldSnapshotId::new(self.world_id.clone(), parent)),
                frontier: generation.frontier,
            })
            .collect();
        for (root, generation) in &inner.generations {
            if rows.iter().any(|row| row.id.root == *root) {
                continue;
            }
            rows.push(WorldGeneration {
                id: WorldSnapshotId::new(self.world_id.clone(), *root),
                parent: inner
                    .parents
                    .get(root)
                    .copied()
                    .flatten()
                    .map(|parent| WorldSnapshotId::new(self.world_id.clone(), parent)),
                frontier: generation.snapshot.frontier(),
            });
        }
        rows.sort_by_key(|row| row.frontier.transaction_count);
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    fn query_publication(
        &self,
        query: Query,
        principal: PrincipalFacts,
        publication: Arc<WorldPublication>,
        world: Arc<dyn World>,
        _world_schemas: Vec<Schema>,
        find_schemas: Vec<crate::find::Schema>,
        _find_extractors: Vec<crate::find::Extractor>,
    ) -> Result<Projection, Failure> {
        self.ensure_live()?;
        let gates = self.context_find_gates_for(&principal, &find_schemas)?;
        let reader = SnapshotReader(publication.snapshot.clone());
        let issued_find_cursor = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let find = crate::world::FindHandle::new(Arc::new(ContextFindReader {
            publication: publication.clone(),
            schemas: Arc::from(find_schemas),
            policy: self.find_policy,
            gates,
            epoch: self.epoch,
            space: self.space.clone(),
            world: self.world_id.clone(),
            implementation: publication.id.publication.implementation_digest,
            actor: principal.actor.clone(),
            device: principal.device.clone(),
            authority_frontier: principal.authority_frontier.clone(),
            issued_cursor: issued_find_cursor.clone(),
        }));
        let mut projection = {
            let principal = &principal;
            let decision = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let ctx = Context::with_world_reads(
                    principal,
                    &reader,
                    publication.id,
                    &self.world_id,
                    find,
                );
                world.query(&ctx, query)
            }))
            .map_err(|_| Failure::CallbackPanicked)?;
            decision.map_err(Failure::Rejected)?
        };
        if issued_find_cursor.load(std::sync::atomic::Ordering::Acquire) {
            let mut inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            inner
                .lease_world_publication(self.world_id.clone(), publication.clone())
                .map_err(|_| Failure::PersistenceCause {
                    operation: "retain World query Find cursor publication",
                    reason: "Station cursor retention capacity exceeded".into(),
                })?;
        }
        // The query's read demand is mandatory and evaluated at the pinned
        // authority frontier — even publicly visible data requires an explicit
        // read capability. No projection is returned on denial.
        if projection.demand.is_empty() {
            return Err(Rejection::ContractViolation.into());
        }
        match self.authority.evaluate_read(
            &principal.actor,
            &principal.authority_frontier,
            &projection.demand,
        ) {
            Ok(true) => {}
            Ok(false) => return Err(Rejection::Denied(DeniedCause::ReadRefused).into()),
            Err(detail) => return Err(Failure::AuthorityUnavailable(detail)),
        }
        // Runtime — not the World — stamps the projection's source frontier: the
        // snapshot it was derived from is the one held for this call.
        projection.frontier = publication.snapshot.frontier();
        projection.publication = Some(publication.id);
        Ok(projection)
    }

    /// Begin observing invalidation records. `None` (or a cursor from another
    /// activation epoch) rebaselines: the stream's first record is a single
    /// reset at the current sequence and committed frontier, after which live
    /// records follow. A cursor from THIS epoch replays every retained record
    /// with a greater sequence, then follows live delivery; a cursor pointing
    /// into a discarded gap yields one reset instead. Records carry Bodies and
    /// the committed frontier — never state; consumers re-query after every
    /// reset. Dormancy ends the stream with a typed error.
    pub fn observe(&self, cursor: Option<ObservationCursor>) -> ObservationStream {
        let position = match cursor {
            Some(c) if c.epoch == self.epoch => Some(c.sequence),
            _ => None,
        };
        ObservationStream {
            broadcaster: self.core.broadcaster.clone(),
            position,
        }
    }

    /// Announce that the Space's **authority** advanced, so subscribers hear it.
    ///
    /// Runtime does not own authority — mechanics does, and a local membership,
    /// role, device or key change never passes through [`Self::submit`]. The
    /// composition root is the only thing holding both, so it is the only thing
    /// that can say this happened. Remote authority arriving over Contact is
    /// published by the driver itself; this is the local half of the same plane.
    ///
    /// Idempotent in effect, not in sequence: each call is one record. Call it
    /// after the authority write is durable, never before — the ordering rule
    /// every publication here obeys.
    pub fn publish_authority_advanced(&self) {
        let frontier = {
            let inner = self.core.lock();
            if inner.closed {
                return;
            }
            inner.replica.frontier()
        };
        self.core
            .broadcaster
            .publish(Vec::new(), frontier, true, Vec::new());
        // And wake anything holding a pinned authority view. A client learns
        // through the Observation above; a delivery plane learns here, because
        // it must not be able to cost a client its cursor by falling behind.
        self.core.note_authority_advanced();
    }

    /// Close this Session, consuming it. Never affects the Station.
    pub fn close(self) {}
}

#[cfg(test)]
mod reservation_tests {
    use super::*;
    use replica::body::{BodyBinding, BodyId, EncodingId};

    const EXEC_TEST_SEED: [u8; 32] = [0x41; 32];

    #[test]
    fn replica_read_does_not_republish_or_move_materialization() {
        let core = StationCore::for_test(replica::Replica::loro());
        let (root, before) = {
            let inner = core.lock();
            (inner.snapshot.root(), inner.snapshot_materialization)
        };

        core.with_replica_read(|_| Ok(())).unwrap();

        let inner = core.lock();
        assert_eq!(inner.snapshot.root(), root);
        assert_eq!(inner.snapshot_materialization, before);
        assert_eq!(
            inner
                .generations
                .get(&root)
                .map(|generation| generation.materialization),
            Some(inner.snapshot_materialization)
        );
    }

    fn key(world: &WorldId, byte: u8) -> BodyKey {
        BodyKey::new(world.clone(), BodyId::from_bytes([byte; 16]))
    }

    fn operation(key: BodyKey) -> (BodyKey, Op) {
        (key, Op::ReplaceAtomic { value: Vec::new() })
    }

    fn effect(operations: Vec<(BodyKey, Op)>) -> Effect {
        Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            operations,
            bodies: Vec::new(),
            effect: Vec::new(),
            declarations: Vec::new(),
            demand: Vec::new(),
        }
    }

    fn binding(schema: &str) -> BodyBinding {
        BodyBinding {
            schema: SchemaId::parse(schema).unwrap(),
            schema_version: 1,
            encoding: EncodingId::parse("bytes").unwrap(),
            mutation_model: replica::body::MUTATION_ATOMIC,
        }
    }

    fn runtime(operations: Vec<(BodyKey, Op)>) -> RuntimeEffect {
        RuntimeEffect {
            bindings: operations
                .iter()
                .map(|(key, _)| (key.clone(), binding(crate::exec::RUN_BODY_SCHEMA)))
                .collect(),
            operations,
            ..RuntimeEffect::default()
        }
    }

    fn demand(name: &str) -> Vec<u8> {
        AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("com.example.product", name),
            mechanics::authorization::Resource::root("com.example.product"),
        )
        .encode_canonical()
        .unwrap()
    }

    fn no_news() -> std::collections::BTreeMap<crate::exec::OfferId, crate::exec::Offer> {
        std::collections::BTreeMap::new()
    }

    fn no_readies() -> std::collections::BTreeMap<crate::exec::OfferId, AcceptedReady> {
        std::collections::BTreeMap::new()
    }

    fn exec_schema(name: &str) -> crate::exec::SchemaRef {
        crate::exec::SchemaRef {
            name: SchemaId::parse(name).unwrap(),
            version: 1,
        }
    }

    fn exec_spec() -> crate::exec::Spec {
        let payload = |name| crate::exec::PayloadSpec {
            schema: exec_schema(name),
            max_inline_bytes: 1_024,
            max_content_refs: 0,
            max_content_bytes: 0,
            read: demand("payload.read"),
            max_additional_input_bytes: 0,
        };
        crate::exec::Spec {
            name: SchemaId::parse("check").unwrap(),
            version: 1,
            access: crate::exec::Access {
                start: demand("start"),
                offer: demand("offer"),
                control: demand("control"),
                accept: demand("accept"),
            },
            input: payload("check.input"),
            output: payload("check.output"),
            mode: crate::exec::Mode::Unary,
            resume: crate::exec::Resume::Restart,
            effects: crate::exec::Effects::Pure,
            accept: crate::exec::AcceptRule::World,
            queries: Vec::new(),
            service: None,
            links: Vec::new(),
            limits: crate::exec::Limits {
                attempts: 2,
                events: 16,
                checkpoints: 0,
                child_runs: 0,
                progress_bytes: 0,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
        }
    }

    fn returned_run_with_spec(
        spec: crate::exec::Spec,
        checkpoint: Option<crate::exec::CheckpointRef>,
        cited_offer: Option<(crate::exec::OfferId, u64)>,
    ) -> (
        replica::Replica,
        Ambient,
        crate::exec::Spec,
        crate::exec::RunId,
        crate::exec::AttemptId,
    ) {
        let world = WorldId::parse("com.example.product").unwrap();
        let space = mechanics::ids::SpaceId::parse("ws_00000000000000000000000000").unwrap();
        let device = mechanics::actor::device_from_seed(&EXEC_TEST_SEED);
        let actor = mechanics::ids::ActorId::from_incept_hash(&"a".repeat(64));
        let station = mechanics::station::Key::from_key_bytes([0x63; 32]);
        let epoch = Epoch::from_u64(2);
        let request = [0x42; 16];
        let run = crate::exec::derive_run_id(&space, &world, &device, request, 0);
        let attempt = crate::exec::AttemptId::from_bytes([0x51; 16]);
        let build = crate::exec::BuildId::from_bytes([0x52; 32]);
        let start = crate::exec::Start {
            spec: crate::exec::SchemaRef {
                name: spec.name.clone(),
                version: spec.version,
            },
            build,
            input: crate::exec::Input {
                inline: b"input".to_vec(),
                content: Vec::new(),
                content_bytes: 0,
            },
            parent: None,
            source: None,
            service: None,
            resources: Vec::new(),
            limits: spec.limits,
            queries: Vec::new(),
        };
        let command = crate::exec::Cmd::Start(start.clone());
        let command_bytes = command.encode().unwrap();
        let started = crate::exec::RunEvent::started(crate::exec::Started {
            space: space.clone(),
            world: world.clone(),
            run,
            spec: start.spec.clone(),
            world_implementation: [0x44; 32],
            build,
            invoker: actor.clone(),
            device: device.clone(),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![1]),
            parent_manifest_root: replica::transaction::NO_PARENT_ROOT,
            input: spec.input.schema.clone(),
            input_digest: start.input_digest(&spec).unwrap(),
            input_content: Vec::new(),
            input_content_bytes: 0,
            resources: Vec::new(),
            limits: spec.limits,
            request,
            command: 0,
            parent: None,
            source: None,
            service: None,
            query_grants_digest: start.query_grants_digest().unwrap(),
            command_digest: command.digest().unwrap(),
            command_bytes: u32::try_from(command_bytes.len()).unwrap(),
            command_chunks: u32::try_from(
                command_bytes
                    .len()
                    .div_ceil(crate::exec::MAX_RUN_COMMAND_CHUNK_BYTES),
            )
            .unwrap(),
        })
        .unwrap();
        let leased = crate::exec::RunEvent::new(
            vec![started.id().unwrap()],
            crate::exec::RunEventKind::Leased(crate::exec::Leased {
                run,
                attempt,
                station: station.clone(),
                station_epoch: epoch,
                executor: actor.clone(),
                device: device.clone(),
                build,
                offer: cited_offer.map(|(id, _)| id),
                offer_epoch: cited_offer.map(|(_, epoch)| epoch).unwrap_or(0),
                resources: Vec::new(),
                enforcement: None,
                limits: crate::exec::AttemptLimits {
                    events: 8,
                    checkpoints: u32::from(checkpoint.is_some()),
                    child_runs: 0,
                    progress_bytes: 0,
                    checkpoint_bytes: if checkpoint.is_some() { 1_024 } else { 0 },
                    wall_millis: 30_000,
                },
                lease: None,
                checkpoint: None,
                fence: crate::exec::Fence::from_u64(1),
            }),
        )
        .unwrap();
        let began = crate::exec::RunEvent::new(
            vec![leased.id().unwrap()],
            crate::exec::RunEventKind::Began(crate::exec::Began {
                run,
                attempt,
                executor: actor.clone(),
                device: device.clone(),
            }),
        )
        .unwrap();
        let mut events = vec![started, leased, began];
        let mut predecessor = events.last().unwrap().id().unwrap();
        if let Some(checkpoint) = checkpoint {
            let saved = crate::exec::RunEvent::new(
                vec![predecessor],
                crate::exec::RunEventKind::Saved(crate::exec::Saved {
                    run,
                    attempt,
                    checkpoint,
                }),
            )
            .unwrap();
            predecessor = saved.id().unwrap();
            events.push(saved);
        }
        let returned = crate::exec::RunEvent::new(
            vec![predecessor],
            crate::exec::RunEventKind::Returned(crate::exec::Returned {
                run,
                attempt,
                output: spec.output.schema.clone(),
                output_digest: [0x55; 32],
                output_inline_bytes: 3,
                output_content: Vec::new(),
                output_content_bytes: 0,
                terminal: crate::exec::TerminalClass::Succeeded,
                usage: Vec::new(),
                evidence: Vec::new(),
            }),
        )
        .unwrap();
        events.push(returned);
        let key = BodyKey::new(world.clone(), BodyId::from_bytes(run.as_bytes()));
        let mut operations = vec![(key.clone(), Op::Create)];
        for (index, event) in events.into_iter().enumerate() {
            operations.push((
                key.clone(),
                Op::ListInsert {
                    path: crate::exec::RUN_EVENTS_PATH.to_string(),
                    index: u64::try_from(index).unwrap(),
                    value: event.encode().unwrap(),
                },
            ));
        }
        for (index, chunk) in command_bytes
            .chunks(crate::exec::MAX_RUN_COMMAND_CHUNK_BYTES)
            .enumerate()
        {
            operations.push((
                key.clone(),
                Op::MapSet {
                    path: crate::exec::RUN_COMMAND_PATH.to_string(),
                    key: format!("{index:08x}"),
                    value: chunk.to_vec(),
                },
            ));
        }
        let mut supported = replica::body::SupportedSchemas::new();
        supported.declare(
            world.clone(),
            SchemaId::parse(crate::exec::RUN_BODY_SCHEMA).unwrap(),
            crate::exec::RUN_BODY_SCHEMA_VERSION,
            EncodingId::parse(crate::exec::BODY_ENCODING).unwrap(),
            replica::body::MUTATION_COLLABORATIVE,
        );
        supported.declare(
            world.clone(),
            SchemaId::parse("product.record").unwrap(),
            1,
            EncodingId::parse("bytes").unwrap(),
            replica::body::MUTATION_ATOMIC,
        );
        let keys = Arc::new(replica::body::StaticBodyKeys::new(
            mechanics::authorization::AuthorizedBodyKey::for_authorized_epoch([1; 16], [2; 32]),
        ));
        let mut replica = replica::Replica::loro().with_keys(keys);
        replica.set_supported(supported);
        let signer = replica::transaction::SeedSigner(&EXEC_TEST_SEED);
        let context = replica::transaction::CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![1]),
        };
        let authorizer = replica::transaction::StaticAuthorizer {
            world: world.clone(),
            implementation_id: [0x44; 32],
        };
        replica
            .commit_action(
                &context,
                &replica::transaction::CommitAuthorization {
                    actor: "actor",
                    parent_manifest_root: replica::transaction::NO_PARENT_ROOT,
                    demand: demand("start"),
                    intent_digest: [3; 32],
                    authorizer: &authorizer,
                },
                &world,
                &device,
                &[4; 16],
                &[5; 32],
                Vec::new(),
                vec![key.clone()],
                "seed-returned",
                &operations,
                &[((key), run_binding().unwrap())],
                &[],
            )
            .unwrap();
        let root = replica.manifest_root();
        let principal = PrincipalFacts {
            actor,
            device,
            station,
            space: space.clone(),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![1]),
        };
        let ambient = Ambient {
            epoch,
            space,
            world,
            implementation: [0x44; 32],
            root,
            extractor_schema_digest: crate::publication::ExtractorSchemaDigest::from_digest(
                [0x55; 32],
            ),
            principal,
            find_policy: crate::find::Policy::default(),
        };
        (replica, ambient, spec, run, attempt)
    }

    fn returned_run() -> (
        replica::Replica,
        Ambient,
        crate::exec::Spec,
        crate::exec::RunId,
        crate::exec::AttemptId,
    ) {
        returned_run_with_spec(
            exec_spec(),
            None,
            Some((crate::exec::OfferId::from_bytes([0x53; 16]), 1)),
        )
    }

    #[test]
    fn raw_world_reads_hide_every_runtime_exec_schema() {
        assert!(world_readable(Some(&binding("product.record"))));
        assert!(!world_readable(None));
        for schema in crate::exec::RESERVED_SCHEMAS {
            assert!(!world_readable(Some(&binding(schema))), "{schema}");
        }
    }

    #[test]
    fn runtime_lowering_is_disjoint_and_shares_the_transaction_cap() {
        let world = WorldId::parse("com.example.product").unwrap();
        let run_key = key(&world, 7);
        let world_effect = effect(vec![operation(run_key.clone())]);
        let lowered = runtime(vec![operation(run_key)]);
        assert_eq!(
            validate_operation_partition(&world, &world_effect, &lowered),
            Err(Rejection::ContractViolation)
        );

        let other_world = WorldId::parse("com.example.other").unwrap();
        assert_eq!(
            validate_operation_partition(
                &world,
                &effect(Vec::new()),
                &runtime(vec![operation(key(&other_world, 1))]),
            ),
            Err(Rejection::ContractViolation)
        );

        let maximum = (0..replica::transaction::MAX_OPS_PER_TRANSACTION)
            .map(|index| operation(key(&world, u8::try_from(index % 256).unwrap())))
            .collect();
        assert_eq!(
            validate_operation_partition(&world, &effect(maximum), &RuntimeEffect::default()),
            Ok(())
        );
        let overflow = vec![operation(key(&world, 2))];
        let maximum_runtime = runtime(
            (0..replica::transaction::MAX_OPS_PER_TRANSACTION)
                .map(|index| operation(key(&world, u8::try_from(index % 256).unwrap())))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            validate_operation_partition(&world, &effect(overflow), &maximum_runtime),
            Err(Rejection::LimitExceeded)
        );
    }

    #[test]
    fn world_and_exec_demands_form_one_canonical_deduplicated_conjunction() {
        let write = demand("write");
        let start = demand("start");
        let combined = combine_demands(&write, &[start.clone(), write.clone()]).unwrap();
        let decoded = AuthorizationDemand::decode_canonical(&combined).unwrap();
        let AuthorizationDemand::All(children) = decoded else {
            panic!("two independent requirements must remain a conjunction");
        };
        assert_eq!(children.len(), 2);
        assert!(children
            .iter()
            .any(|child| child.encode_canonical().unwrap() == write));
        assert!(children
            .iter()
            .any(|child| child.encode_canonical().unwrap() == start));

        assert_eq!(
            combine_demands(&write, std::slice::from_ref(&write)),
            Ok(write)
        );
        assert_eq!(combine_demands(&[], &[]), Err(Rejection::ContractViolation));
    }

    #[test]
    fn outcome_validation_and_product_mutation_share_one_acceptance_transaction() {
        let (mut replica, ambient, spec, run, attempt) = returned_run();
        let pinned = replica.read_snapshot();
        let reader = ReplicaReader {
            replica: &replica,
            snapshot: &pinned,
        };
        let context = Context::with_world_reads_for_test(
            &ambient.principal,
            &reader,
            pinned.root(),
            &ambient.world,
        );
        let facts = context
            .outcome(run, attempt)
            .expect("the exact returned Attempt is visible to the World");
        assert_eq!(facts.spec.name, spec.name);
        assert_eq!(facts.output, spec.output.schema);
        assert_eq!(facts.output_digest, [0x55; 32]);
        assert!(facts.returned_exactly_once);

        let lowered = lower_exec(
            &[crate::exec::Cmd::Accept { run, attempt }],
            std::slice::from_ref(&spec),
            &ambient,
            [0x61; 16],
            1,
            &pinned,
            &OfferAdmission {
                news: &no_news(),
                readies: &no_readies(),
                now_millis: 1,
            },
        )
        .unwrap();
        assert_eq!(lowered.operations.len(), 1);
        let Op::ListInsert { value, .. } = &lowered
            .operations
            .first()
            .expect("one acceptance operation")
            .1
        else {
            panic!("acceptance lowers to one protected event insertion");
        };
        assert!(matches!(
            crate::exec::RunEvent::decode_canonical(value).unwrap().kind,
            crate::exec::RunEventKind::Accepted(_)
        ));

        let product = BodyKey::new(ambient.world.clone(), BodyId::from_bytes([0x62; 16]));
        let product_binding = BodyBinding {
            schema: SchemaId::parse("product.record").unwrap(),
            schema_version: 1,
            encoding: EncodingId::parse("bytes").unwrap(),
            mutation_model: replica::body::MUTATION_ATOMIC,
        };
        let world_effect = Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            operations: vec![(
                product.clone(),
                Op::ReplaceAtomic {
                    value: facts.output_digest.to_vec(),
                },
            )],
            bodies: vec![product.clone()],
            effect: Vec::new(),
            declarations: Vec::new(),
            demand: demand("product.accept"),
        };
        assert_eq!(
            validate_operation_partition(&ambient.world, &world_effect, &lowered),
            Ok(())
        );

        let mut operations = world_effect.operations.clone();
        operations.extend(lowered.operations.clone());
        let mut bindings = vec![(product.clone(), product_binding)];
        bindings.extend(lowered.bindings.clone());
        let mut bodies = world_effect.bodies.clone();
        bodies.extend(lowered.bodies.clone());
        let demand = combine_demands(&world_effect.demand, &lowered.demands).unwrap();
        let signer = replica::transaction::SeedSigner(&EXEC_TEST_SEED);
        let commit = replica::transaction::CommitContext {
            space: &ambient.space,
            signer: &signer,
            authority_frontier: ambient.principal.authority_frontier.clone(),
        };
        let authorizer = replica::transaction::StaticAuthorizer {
            world: ambient.world.clone(),
            implementation_id: ambient.implementation,
        };
        replica
            .commit_action(
                &commit,
                &replica::transaction::CommitAuthorization {
                    actor: "actor",
                    parent_manifest_root: pinned.root(),
                    demand,
                    intent_digest: [0x63; 32],
                    authorizer: &authorizer,
                },
                &ambient.world,
                &ambient.principal.device,
                &[0x64; 16],
                &[0x65; 32],
                Vec::new(),
                bodies,
                "accept",
                &operations,
                &bindings,
                &lowered.content_refs,
            )
            .unwrap();

        assert_eq!(replica.read(&product), Some(facts.output_digest.to_vec()));
        let snapshot = replica.read_snapshot();
        let (accepted, _, _) = crate::exec::read_committed_run(&snapshot, &ambient.world, run)
            .unwrap()
            .unwrap();
        assert_eq!(accepted.accepted.len(), 1);
        assert_eq!(
            accepted
                .accepted
                .first()
                .expect("one accepted fact")
                .value
                .attempt,
            attempt
        );
        assert!(!accepted.is_unresolved());

        assert!(matches!(
            lower_exec(
                &[
                    crate::exec::Cmd::Accept { run, attempt },
                    crate::exec::Cmd::Reject { run, attempt },
                ],
                std::slice::from_ref(&spec),
                &ambient,
                [0x66; 16],
                0,
                &pinned,
                &OfferAdmission {
                    news: &no_news(),
                    readies: &no_readies(),
                    now_millis: 1,
                },
            ),
            Err(Rejection::ContractViolation)
        ));
    }

    #[test]
    fn work_continue_derives_a_new_attempt_from_committed_scheduling_evidence() {
        let (replica, ambient, spec, run, prior_attempt) = returned_run();
        let pinned = replica.read_snapshot();
        let request = crate::exec::WorkRequest::Retry {
            world: ambient.world.clone(),
            run,
        };

        let intent = continuation_try(&pinned, std::slice::from_ref(&spec), &ambient, &request)
            .expect("continue should derive a bounded Try from the committed Attempt");
        assert_eq!(intent.run, run);
        assert_eq!(intent.build, crate::exec::BuildId::from_bytes([0x52; 32]));
        let offer = intent.offer.as_ref().expect("continued offer");
        assert_eq!(offer.id, crate::exec::OfferId::from_bytes([0x53; 16]));
        assert_eq!(offer.station, ambient.principal.station);
        assert_eq!(offer.station_epoch, ambient.epoch);
        assert_eq!(offer.epoch, 1);
        assert_eq!(intent.enforcement, None);
        assert_eq!(intent.fence, crate::exec::Fence::from_u64(2));
        assert!(intent.checkpoint.is_none());

        let command_request = [0x76; 16];
        let lowered = lower_exec(
            &[crate::exec::Cmd::Try(intent)],
            std::slice::from_ref(&spec),
            &ambient,
            command_request,
            0,
            &pinned,
            &OfferAdmission {
                news: &no_news(),
                readies: &no_readies(),
                now_millis: 1,
            },
        )
        .expect("the derived Try should satisfy Runtime admission");
        let Op::ListInsert { value, .. } =
            &lowered.operations.first().expect("one lease operation").1
        else {
            panic!("continue must lower to a visible Attempt");
        };
        let event = crate::exec::RunEvent::decode_canonical(value).unwrap();
        let crate::exec::RunEventKind::Leased(leased) = event.kind else {
            panic!("continue must commit a Leased event");
        };
        assert_ne!(leased.attempt, prior_attempt);
        assert_eq!(
            leased.attempt,
            crate::exec::derive_attempt_id(run, &ambient.principal.device, command_request, 0)
        );
        assert_eq!(leased.fence, crate::exec::Fence::from_u64(2));

        let resume = crate::exec::WorkRequest::Resume {
            world: ambient.world.clone(),
            run,
            checkpoint: replica::content::ContentRef {
                content_id: [0x77; 32],
            },
        };
        assert_eq!(
            continuation_try(&pinned, &[spec], &ambient, &resume),
            Err(crate::exec::WorkRefusal::Unsupported(
                "this Run restarts rather than resuming from a checkpoint; use continue"
            ))
        );
    }

    #[test]
    fn work_resume_derives_a_new_attempt_from_the_exact_committed_checkpoint() {
        let mut spec = exec_spec();
        spec.resume = crate::exec::Resume::Checkpoint {
            codec: exec_schema("check.checkpoint"),
        };
        spec.limits.checkpoints = 1;
        spec.limits.checkpoint_bytes = 1_024;
        let checkpoint = crate::exec::CheckpointRef {
            content: replica::content::ContentRef {
                content_id: [0x78; 32],
            },
            build: crate::exec::BuildId::from_bytes([0x52; 32]),
            sequence: 1,
        };
        let (replica, ambient, spec, run, prior_attempt) = returned_run_with_spec(
            spec,
            Some(checkpoint.clone()),
            Some((crate::exec::OfferId::from_bytes([0x53; 16]), 1)),
        );
        let pinned = replica.read_snapshot();
        let request = crate::exec::WorkRequest::Resume {
            world: ambient.world.clone(),
            run,
            checkpoint: checkpoint.content,
        };

        let intent = continuation_try(&pinned, std::slice::from_ref(&spec), &ambient, &request)
            .expect("resume should derive a bounded Try from the exact checkpoint");
        assert_eq!(intent.checkpoint, Some(checkpoint.clone()));
        assert_eq!(intent.fence, crate::exec::Fence::from_u64(2));

        let command_request = [0x79; 16];
        let lowered = lower_exec(
            &[crate::exec::Cmd::Try(intent)],
            &[spec],
            &ambient,
            command_request,
            0,
            &pinned,
            &OfferAdmission {
                news: &no_news(),
                readies: &no_readies(),
                now_millis: 1,
            },
        )
        .expect("the checkpoint-derived Try should satisfy Runtime admission");
        let Op::ListInsert { value, .. } =
            &lowered.operations.first().expect("one lease operation").1
        else {
            panic!("resume must lower to a visible Attempt");
        };
        let event = crate::exec::RunEvent::decode_canonical(value).unwrap();
        let crate::exec::RunEventKind::Leased(leased) = event.kind else {
            panic!("resume must commit a Leased event");
        };
        assert_ne!(leased.attempt, prior_attempt);
        assert_eq!(leased.checkpoint, Some(checkpoint));
        assert_eq!(leased.fence, crate::exec::Fence::from_u64(2));
    }

    #[test]
    fn local_try_mints_a_new_attempt_and_cancel_records_only_a_request() {
        let (replica, ambient, spec, run, prior_attempt) = returned_run();
        let pinned = replica.read_snapshot();
        let request = [0x71; 16];
        let intent = crate::exec::Try {
            run,
            build: crate::exec::BuildId::from_bytes([0x52; 32]),
            offer: None,
            resources: Vec::new(),
            enforcement: None,
            limits: crate::exec::AttemptLimits {
                events: 8,
                checkpoints: 0,
                child_runs: 0,
                progress_bytes: 0,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
            lease: None,
            checkpoint: None,
            fence: crate::exec::Fence::from_u64(2),
        };
        let retry = lower_exec(
            &[crate::exec::Cmd::Try(intent.clone())],
            std::slice::from_ref(&spec),
            &ambient,
            request,
            0,
            &pinned,
            &OfferAdmission {
                news: &no_news(),
                readies: &no_readies(),
                now_millis: 1,
            },
        )
        .unwrap();
        let Op::ListInsert { value, .. } =
            &retry.operations.first().expect("one lease operation").1
        else {
            panic!("Try lowers to a lease event");
        };
        let event = crate::exec::RunEvent::decode_canonical(value).unwrap();
        let crate::exec::RunEventKind::Leased(leased) = event.kind else {
            panic!("Try must create a visible Attempt");
        };
        assert_ne!(leased.attempt, prior_attempt);
        assert_eq!(
            leased.attempt,
            crate::exec::derive_attempt_id(run, &ambient.principal.device, request, 0)
        );
        assert_eq!(leased.run, run);
        assert_eq!(leased.build, crate::exec::BuildId::from_bytes([0x52; 32]));
        assert!(matches!(
            lower_exec(
                &[
                    crate::exec::Cmd::Try(intent.clone()),
                    crate::exec::Cmd::Try(intent),
                ],
                std::slice::from_ref(&spec),
                &ambient,
                [0x75; 16],
                0,
                &pinned,
                &OfferAdmission {
                    news: &no_news(),
                    readies: &no_readies(),
                    now_millis: 1,
                },
            ),
            Err(Rejection::ContractViolation)
        ));

        let cancelled = lower_exec(
            &[crate::exec::Cmd::Cancel { run }],
            &[spec],
            &ambient,
            [0x74; 16],
            0,
            &pinned,
            &OfferAdmission {
                news: &no_news(),
                readies: &no_readies(),
                now_millis: 1,
            },
        )
        .unwrap();
        let Op::ListInsert { value, .. } = &cancelled
            .operations
            .first()
            .expect("one cancellation operation")
            .1
        else {
            panic!("Cancel lowers to one lifecycle event");
        };
        let event = crate::exec::RunEvent::decode_canonical(value).unwrap();
        assert!(matches!(
            &event.kind,
            crate::exec::RunEventKind::CancelAsked(_)
        ));
        assert!(!matches!(
            &event.kind,
            crate::exec::RunEventKind::Cancelled(_)
        ));
    }

    fn signed_offer(
        ambient: &Ambient,
        build: crate::exec::BuildId,
        epoch: u64,
        expiry: u64,
    ) -> crate::exec::Offer {
        crate::exec::Offer {
            id: crate::exec::OfferId::from_bytes([0; 16]),
            space: ambient.space.clone(),
            station: ambient.principal.station.clone(),
            station_epoch: ambient.epoch,
            actor: ambient.principal.actor.clone(),
            device: ambient.principal.device.clone(),
            world: ambient.world.clone(),
            world_build: ambient.implementation,
            builds: vec![crate::exec::OfferedBuild {
                id: build,
                spec: exec_schema("check"),
            }],
            resources: vec![crate::exec::Resource {
                name: SchemaId::parse(crate::exec::MEMORY_BYTES).unwrap(),
                amount: 65_536,
            }],
            backend: SchemaId::parse("in-process.rust").unwrap(),
            enforcement: crate::exec::Enforcement::Advisory,
            resident: Vec::new(),
            availability: crate::exec::Availability::Ready,
            epoch,
            expiry,
            publisher: ambient.principal.actor.clone(),
            signature: crate::exec::Signature {
                signer: mechanics::actor::device_from_seed(&EXEC_TEST_SEED),
                algorithm: 1,
                bytes: [0; 64],
            },
        }
        .sign(&EXEC_TEST_SEED)
        .unwrap()
    }

    fn try_for_offer(
        run: crate::exec::RunId,
        build: crate::exec::BuildId,
        offer: Option<crate::exec::OfferRef>,
    ) -> crate::exec::Try {
        crate::exec::Try {
            run,
            build,
            offer,
            resources: Vec::new(),
            enforcement: None,
            limits: crate::exec::AttemptLimits {
                events: 8,
                checkpoints: 0,
                child_runs: 0,
                progress_bytes: 0,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
            lease: None,
            checkpoint: None,
            fence: crate::exec::Fence::from_u64(2),
        }
    }

    fn ready_for(ambient: &Ambient, offer: &crate::exec::Offer, now: u64) -> AcceptedReady {
        let challenge = crate::exec::Challenge {
            offer: offer.id,
            nonce: [0x21; 16],
            station: ambient.principal.station.clone(),
            station_epoch: ambient.epoch,
            issued_at: now.max(1),
            expiry: offer
                .expiry
                .min(now.saturating_add(crate::exec::CHALLENGE_TTL_MILLIS)),
        };
        let ready = crate::exec::Ready::sign(&challenge, &EXEC_TEST_SEED).unwrap();
        AcceptedReady { challenge, ready }
    }

    #[test]
    fn first_use_try_requires_live_offer_news_and_ready() {
        let (replica, ambient, spec, run, _) = returned_run();
        let pinned = replica.read_snapshot();
        let build = crate::exec::BuildId::from_bytes([0x52; 32]);
        let offer = signed_offer(&ambient, build, 9, 10_000);
        let intent = try_for_offer(run, build, Some(offer.reference()));

        assert!(matches!(
            lower_exec(
                &[crate::exec::Cmd::Try(intent.clone())],
                std::slice::from_ref(&spec),
                &ambient,
                [0x81; 16],
                0,
                &pinned,
                &OfferAdmission {
                    news: &no_news(),
                    readies: &no_readies(),
                    now_millis: 1,
                },
            ),
            Err(Rejection::ContractViolation)
        ));

        let mut news = no_news();
        news.insert(offer.id, offer.clone());
        assert!(matches!(
            lower_exec(
                &[crate::exec::Cmd::Try(intent.clone())],
                std::slice::from_ref(&spec),
                &ambient,
                [0x82; 16],
                0,
                &pinned,
                &OfferAdmission {
                    news: &news,
                    readies: &no_readies(),
                    now_millis: 1,
                },
            ),
            Err(Rejection::ContractViolation)
        ));

        let mut readies = no_readies();
        readies.insert(offer.id, ready_for(&ambient, &offer, 1));
        let lowered = lower_exec(
            &[crate::exec::Cmd::Try(intent)],
            std::slice::from_ref(&spec),
            &ambient,
            [0x83; 16],
            0,
            &pinned,
            &OfferAdmission {
                news: &news,
                readies: &readies,
                now_millis: 1,
            },
        )
        .expect("live Offer plus Ready admits a first-use Try");
        assert!(lowered
            .demands
            .iter()
            .any(|demand| demand == &spec.access.offer));

        let stale = signed_offer(&ambient, build, 10, 2);
        let mut expired = no_news();
        expired.insert(stale.id, stale.clone());
        let mut expired_ready = no_readies();
        expired_ready.insert(stale.id, ready_for(&ambient, &stale, 1));
        assert!(matches!(
            lower_exec(
                &[crate::exec::Cmd::Try(try_for_offer(
                    run,
                    build,
                    Some(stale.reference()),
                ))],
                std::slice::from_ref(&spec),
                &ambient,
                [0x84; 16],
                0,
                &pinned,
                &OfferAdmission {
                    news: &expired,
                    readies: &expired_ready,
                    now_millis: 2,
                },
            ),
            Err(Rejection::ContractViolation)
        ));
    }

    #[test]
    fn a_false_or_stale_offer_does_not_block_a_station_only_try() {
        let (replica, ambient, spec, run, _) = returned_run();
        let pinned = replica.read_snapshot();
        let build = crate::exec::BuildId::from_bytes([0x52; 32]);
        let offer = signed_offer(&ambient, build, 11, 10_000);
        let mut news = no_news();
        news.insert(offer.id, offer.clone());
        let mut readies = no_readies();
        readies.insert(offer.id, ready_for(&ambient, &offer, 1));

        let lowered = lower_exec(
            &[crate::exec::Cmd::Try(try_for_offer(run, build, None))],
            std::slice::from_ref(&spec),
            &ambient,
            [0x85; 16],
            0,
            &pinned,
            &OfferAdmission {
                news: &news,
                readies: &readies,
                now_millis: 1,
            },
        )
        .expect("Station-only Try remains legal while unused Offer news is held");
        let Op::ListInsert { value, .. } = &lowered.operations.first().expect("one lease").1 else {
            panic!("Station-only Try must still lease");
        };
        let event = crate::exec::RunEvent::decode_canonical(value).unwrap();
        let crate::exec::RunEventKind::Leased(leased) = event.kind else {
            panic!("Station-only Try must commit Leased");
        };
        assert!(leased.offer.is_none());
    }

    #[test]
    fn continue_with_live_offer_news_does_not_need_a_new_ready() {
        let build = crate::exec::BuildId::from_bytes([0x52; 32]);
        let (_, ambient, spec, _, _) = returned_run();
        let offer = signed_offer(&ambient, build, 12, 10_000);
        let (replica, ambient, spec, run, _) =
            returned_run_with_spec(spec, None, Some((offer.id, offer.epoch)));
        let pinned = replica.read_snapshot();
        let request = crate::exec::WorkRequest::Retry {
            world: ambient.world.clone(),
            run,
        };
        let intent = continuation_try(&pinned, std::slice::from_ref(&spec), &ambient, &request)
            .expect("continue cites the historical Offer");
        assert_eq!(intent.offer.as_ref().map(|offer| offer.id), Some(offer.id));

        let mut news = no_news();
        news.insert(offer.id, offer);
        let lowered = lower_exec(
            &[crate::exec::Cmd::Try(intent)],
            std::slice::from_ref(&spec),
            &ambient,
            [0x86; 16],
            0,
            &pinned,
            &OfferAdmission {
                news: &news,
                readies: &no_readies(),
                now_millis: 1,
            },
        )
        .expect("live news must not demand a new Ready on continue");
        assert!(lowered
            .demands
            .iter()
            .any(|demand| demand == &spec.access.offer));
    }

    #[test]
    fn two_first_use_tries_cannot_share_one_ready() {
        let mut spec = exec_spec();
        spec.limits.attempts = 4;
        let (replica, ambient, spec, run, _) = returned_run_with_spec(spec, None, None);
        let pinned = replica.read_snapshot();
        let build = crate::exec::BuildId::from_bytes([0x52; 32]);
        let offer = signed_offer(&ambient, build, 13, 10_000);
        let mut news = no_news();
        news.insert(offer.id, offer.clone());
        let mut readies = no_readies();
        readies.insert(offer.id, ready_for(&ambient, &offer, 1));
        let first = try_for_offer(run, build, Some(offer.reference()));
        let second = try_for_offer(run, build, Some(offer.reference()));
        assert!(matches!(
            lower_exec(
                &[crate::exec::Cmd::Try(first), crate::exec::Cmd::Try(second),],
                std::slice::from_ref(&spec),
                &ambient,
                [0x87; 16],
                0,
                &pinned,
                &OfferAdmission {
                    news: &news,
                    readies: &readies,
                    now_millis: 1,
                },
            ),
            Err(Rejection::ContractViolation)
        ));
    }
}
