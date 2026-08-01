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

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use mechanics::station::Epoch;
use replica::body::{BodyKey, SchemaId, WorldId};
use replica::body::{MutationModel, Op, Schema};
use replica::frontier::ReplicaFrontier;
use serde::{Deserialize, Serialize};

use crate::world::{
    AuthorityView, Context, Effect, Intent, Limits, PrincipalFacts, Projection, Query, Rejection,
    World,
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
    Reset,
    CallbackPanicked,
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
}

/// The single mutex-guarded committing state: the Replica writer plus the
/// closed flag. `closed` lives **inside** the same mutex as the writer so that
/// commit admission and Station shutdown are one serialized state transition —
/// a submit admitted before dormancy either commits before the close (and is
/// durable + checkpointed) or observes `closed` and is refused. There is no
/// window where a commit lands after the shutdown checkpoint.
struct CoreInner {
    replica: replica::Replica,
    closed: bool,
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
    pub(crate) fn publish(&self, bodies: Vec<BodyKey>, frontier: ReplicaFrontier, authority: bool) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return;
        }
        let sequence = state.next_seq;
        state.next_seq += 1;
        state.last_frontier = frontier;
        let record = Observation {
            epoch: self.epoch,
            sequence,
            reset: false,
            bodies,
            authority,
            frontier,
        };
        if state.ring.len() == state.capacity {
            state.ring.pop_front();
        }
        state.ring.push_back(record);
        self.wake.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
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
        }
    }

    /// The next record, waiting up to `timeout`. `Ok(None)` on timeout;
    /// [`Interruption::StationDormant`] once the Station closed.
    pub fn next_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<Observation>, Interruption> {
        let deadline = std::time::Instant::now() + timeout;
        let broadcaster = self.broadcaster.clone();
        let mut state = broadcaster.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if state.closed {
                return Err(Interruption::StationDormant);
            }
            if let Some(record) = self.pull(&state) {
                self.position = Some(record.sequence);
                return Ok(Some(record));
            }
            let now = std::time::Instant::now();
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
        Self {
            inner: std::sync::Mutex::new(CoreInner {
                replica,
                closed: false,
            }),
            broadcaster: Arc::new(Broadcaster::new(epoch, observation_capacity, frontier)),
            authority_tick: tokio::sync::watch::Sender::new(0),
        }
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
        self.authority_tick
            .send_modify(|n| *n = n.saturating_add(1));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CoreInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn frontier(&self) -> ReplicaFrontier {
        self.lock().replica.frontier()
    }

    /// Run a closure against the exclusive Replica writer (the Contact plane's
    /// snapshot/incorporation entry). Refused once the core is closed.
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
    pub fn with_replica<T>(
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

/// The Live plane's caret reads, at the price `with_replica` documents.
///
/// Implemented here rather than in `live.rs` because this is the type that owns
/// the lock: a caller reading these two methods sees `with_replica` one line
/// away and the paragraph explaining why it is exclusive one line after that.
impl crate::plane::live::AnchorSource for StationCore {
    fn anchor_in_body(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        // A dormant core answers `None`, which is the same answer a position
        // the algebra cannot bind gets. Both mean there is no anchor to send,
        // and a caller has nothing different to do about them.
        self.with_replica(|replica| Ok(replica.anchor(key, path, position)))
            .ok()
            .flatten()
    }

    fn resolve_anchor(&self, key: &BodyKey, anchor: &fabric::Anchor) -> fabric::AnchorResolution {
        // Total, so a dormant core is `Drifted` rather than an error — the
        // renderer's contract is that this never fails and never lies, not that
        // it always knows.
        self.with_replica(|replica| Ok(replica.resolve_anchor(key, anchor)))
            .unwrap_or(fabric::AnchorResolution::Drifted)
    }
}

/// A [`BodyReader`] over a locked Replica, handed to a World during a query.
struct ReplicaReader<'a>(&'a replica::Replica);

impl crate::world::BodyReader for ReplicaReader<'_> {
    fn read_body(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.0.read(key)
    }
    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        self.0.read_collaborative(key)
    }
    fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        self.0.body_version(key)
    }
    fn anchor_in_body(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        self.0.anchor(key, path, position)
    }
    fn resolve_anchor(&self, key: &BodyKey, anchor: &fabric::Anchor) -> fabric::AnchorResolution {
        self.0.resolve_anchor(key, anchor)
    }
    fn content_status(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<crate::world::ContentStatus> {
        // Residency is the host's question, not the Replica's, so a World
        // reading through a committed snapshot sees geometry with zero
        // residency. The host surface is where "how much is here" is answered,
        // because that is where the cache is.
        self.0
            .content_descriptor(content)
            .map(|d| crate::world::ContentStatus {
                plaintext_len: d.plaintext_len,
                chunk_count: d.chunk_count,
                resident_chunks: 0,
            })
    }
    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        self.0
            .body_keys()
            .into_iter()
            .filter(|k| &k.world == world && self.0.binding(k).is_some_and(|b| &b.schema == schema))
            .collect()
    }
    fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.0.body_stamp(key)
    }
}

/// A local caller's handle to a hosted World.
pub struct Session {
    space: mechanics::ids::SpaceId,
    world_id: WorldId,
    world: Arc<dyn World>,
    /// The docked identity: signs this Session's durable Body transactions.
    identity: crate::world::LocalIdentity,
    principal: PrincipalFacts,
    epoch: Epoch,
    /// The World's declared limits, enforced before the callback runs.
    limits: Limits,
    /// The World's declared schemas, checked against each request.
    schemas: Vec<Schema>,
    /// A shared flag: `false` once the Station is going dormant or has exited.
    /// A Session only *reads* it — it can never stop the Station.
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The Station's exclusive committing state.
    core: Arc<StationCore>,
    /// The mechanics authority view: standing is re-resolved per request and the
    /// authority frontier is compare-and-swapped at commit.
    authority: Arc<dyn AuthorityView>,
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
        alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
        core: Arc<StationCore>,
        authority: Arc<dyn AuthorityView>,
    ) -> Self {
        Self {
            space,
            world_id,
            world,
            identity,
            principal,
            epoch,
            limits,
            schemas,
            alive,
            core,
            authority,
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
            .ok_or(Rejection::Denied)?;
        Ok(PrincipalFacts {
            actor: resolution.actor,
            device: self.principal.device.clone(),
            station: self.principal.station.clone(),
            space: self.space.clone(),
            authority_frontier: resolution.authority_frontier,
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
    ) -> Result<Vec<(BodyKey, replica::body::BodyBinding)>, Rejection> {
        if effect.operations.len() > replica::transaction::MAX_OPS_PER_TRANSACTION {
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
                .ok_or(Rejection::ContractViolation)?;
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
        let mut saw_schema = false;
        for s in &self.schemas {
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
            return Err(Rejection::Denied.into());
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
                return Ok(CommittedEffect {
                    effect: receipt.effect,
                    frontier: receipt.frontier,
                    bodies: receipt.bodies,
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
        let effect: Effect = {
            let reader = ReplicaReader(&inner.replica);
            let parent_root = inner.replica.manifest_root();
            let principal = &principal;
            let decision = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let mut ctx = Context::with_reads(principal, &reader, parent_root);
                world.submit(&mut ctx, intent)
            }))
            .map_err(|_| Failure::CallbackPanicked)?;
            decision.map_err(Failure::Rejected)?
        };
        // Contain the staged effect inside this World's namespace and each
        // Body's exact schema binding, resolving the bindings the commit is
        // made under.
        let bindings = self.contain_effect(&inner.replica, &effect, &intent_schema)?;
        // Authority-frontier compare-and-swap, still under the writer lock:
        // the frontier the request was authorized at must still be current at
        // commit. A change refuses the commit with AuthorityChanged and
        // commits nothing.
        let current = self
            .authority
            .resolve(&principal.device)
            .ok_or(Rejection::Denied)?;
        if current.authority_frontier != action.header.authority_frontier {
            return Err(Failure::Conflict(Conflict::AuthorityChanged));
        }
        // The mutation's canonical demand is mandatory and non-empty; the
        // implementation must be active at the pinned frontier.
        if effect.demand.is_empty() {
            return Err(Rejection::ContractViolation.into());
        }
        let implementation_id = self
            .authority
            .active_implementation(&self.world_id, &action.header.authority_frontier)
            .ok_or(Rejection::Denied)?;
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
            demand: effect.demand.clone(),
            intent_digest: payload_hash,
            authorizer: &authorizer,
        };
        let outcome = inner
            .replica
            .commit_action(
                &ctx,
                &auth,
                &self.world_id,
                &principal.device,
                &request,
                &payload_hash,
                effect.effect,
                effect.bodies,
                &label,
                &effect.operations,
                &bindings,
                &effect.content_refs,
            )
            .map_err(|e| match e {
                // A staged op the engine cannot express is a World bug.
                replica::transaction::commit::Failure::UnsupportedOp => {
                    Failure::Rejected(Rejection::ContractViolation)
                }
                replica::transaction::commit::Failure::PathInvalid
                | replica::transaction::commit::Failure::InvalidOp(_) => {
                    Failure::Rejected(Rejection::InvalidRequest)
                }
                replica::transaction::commit::Failure::OpLimit => {
                    Failure::Rejected(Rejection::LimitExceeded)
                }
                replica::transaction::commit::Failure::EffectTooLarge => {
                    Failure::Rejected(Rejection::LimitExceeded)
                }
                replica::transaction::commit::Failure::TypeConflict => {
                    Failure::Conflict(Conflict::Body)
                }
                replica::transaction::commit::Failure::SchemaMismatch => {
                    Failure::Rejected(Rejection::ContractViolation)
                }
                replica::transaction::commit::Failure::RequestIdConflict => {
                    Failure::Conflict(Conflict::Request)
                }
                replica::transaction::commit::Failure::QuotaExceeded
                | replica::transaction::commit::Failure::OpaqueQuotaExceeded => {
                    Failure::Rejected(Rejection::LimitExceeded)
                }
                // The mechanics authorizer refused: the demand was unsatisfied
                // at the pinned frontier (a real Denied, not a bug).
                replica::transaction::commit::Failure::Unauthorized(_) => {
                    Failure::Rejected(Rejection::Denied)
                }
                replica::transaction::commit::Failure::ParentManifestUnavailable => {
                    Failure::Conflict(Conflict::Body)
                }
                // Illegitimate is an incorporation-path error; a local commit
                // never produces it, but the match stays exhaustive.
                replica::transaction::commit::Failure::Illegitimate(_)
                | replica::transaction::commit::Failure::Engine(_)
                | replica::transaction::commit::Failure::Integrity(_)
                | replica::transaction::commit::Failure::Body(_)
                | replica::transaction::commit::Failure::BodyKeyUnavailable
                | replica::transaction::commit::Failure::Durability(_)
                | replica::transaction::commit::Failure::OutcomeUnknown
                | replica::transaction::commit::Failure::Poisoned => Failure::Persistence,
            })?;
        // Publish the Observation for a FRESH durable commit while still
        // holding the writer lock: publication order equals commit order, and
        // nothing is ever published before durability. A replay publishes
        // nothing (nothing committed).
        if let replica::transaction::ActionOutcome::Committed(receipt) = &outcome {
            self.core
                .broadcaster
                .publish(receipt.bodies.clone(), receipt.frontier, false);
        }
        drop(inner);
        let receipt = match outcome {
            replica::transaction::ActionOutcome::Committed(r)
            | replica::transaction::ActionOutcome::Replayed(r) => r,
        };
        Ok(CommittedEffect {
            effect: receipt.effect,
            frontier: receipt.frontier,
            bodies: receipt.bodies,
        })
    }

    /// Query the World over the stable committed snapshot. The World reads
    /// committed Bodies through the bounded context; the snapshot is held for the
    /// duration of the call so the projection is derived from one consistent
    /// frontier.
    pub fn query(&self, query: Query) -> Result<Projection, Failure> {
        self.ensure_live()?;
        self.ensure_within_limit(query.payload.len())?;
        self.ensure_readable_schema(&query.schema, query.schema_version)?;
        // Per-request authorization for reads as well.
        let principal = self.fresh_principal()?;
        let world = &self.world;
        let inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
        let reader = ReplicaReader(&inner.replica);
        let snapshot_root = inner.replica.manifest_root();
        let mut projection = {
            let principal = &principal;
            let decision = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let ctx = Context::with_reads(principal, &reader, snapshot_root);
                world.query(&ctx, query)
            }))
            .map_err(|_| Failure::CallbackPanicked)?;
            decision.map_err(Failure::Rejected)?
        };
        // The query's read demand is mandatory and evaluated at the pinned
        // authority frontier — even publicly visible data requires an explicit
        // read capability. No projection is returned on denial.
        if projection.demand.is_empty() {
            return Err(Rejection::ContractViolation.into());
        }
        if !self.authority.evaluate_read(
            &principal.actor,
            &principal.authority_frontier,
            &projection.demand,
        ) {
            return Err(Rejection::Denied.into());
        }
        // Runtime — not the World — stamps the projection's source frontier: the
        // snapshot it was derived from is the one held for this call.
        projection.frontier = inner.replica.frontier();
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
        self.core.broadcaster.publish(Vec::new(), frontier, true);
        // And wake anything holding a pinned authority view. A client learns
        // through the Observation above; a delivery plane learns here, because
        // it must not be able to cost a client its cursor by falling behind.
        self.core.note_authority_advanced();
    }

    /// Close this Session, consuming it. Never affects the Station.
    pub fn close(self) {}
}
