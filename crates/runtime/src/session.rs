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
    AuthorityView, Context, DeniedCause, Effect, Intent, LifecycleSourceCoordinate, Limits,
    PrincipalFacts, Projection, Query, Rejection, World,
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
    /// The authoritative journal commit crossed a point where durability could
    /// no longer be determined. The caller must reopen and look up the same
    /// operation id; blind retry could duplicate an operation that did commit.
    OutcomeUnknown,
    /// Another durable mutation already owns this Station's bounded writer
    /// lane. The request has not been admitted and may be retried after the
    /// current operation publishes or is refused.
    Busy,
    Reset,
    CallbackPanicked,
    /// The requested World generation is well-formed but this Station does not
    /// retain its material. Distinct from reset/interruption: callers may fall
    /// back to a nearer ancestor or request the generation from another holder.
    GenerationUnavailable,
    /// The composition-wide immutable read/build envelope cannot admit this
    /// additional publication without jeopardising already-admitted work.
    ReadCapacity,
    /// A Station-local exact publication coordinate is no longer retained.
    /// Runtime never substitutes another materialization of the same portable
    /// publication: callers must reconcile from a newer acknowledged WPI.
    PublicationExpired(crate::publication::WorldPublicationId),
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
    /// The signed persistent operation coordinate whose durable receipt this
    /// effect represents. Human and agent access paths receive this same value,
    /// and the corresponding Observation attribution must match it exactly.
    pub operation: [u8; 16],
    pub effect: Vec<u8>,
    pub frontier: ReplicaFrontier,
    pub bodies: Vec<BodyKey>,
    /// The exact publication prepared before durability and installed with the
    /// acknowledgement. Idempotent replay reads these semantic coordinates
    /// from the durable receipt rather than mutable current package state.
    pub publication: crate::publication::WorldPublicationId,
}

/// Durable result material recovered by the read-only operation-status path.
/// It is authoritative even when this activation cannot currently construct
/// the exact World read image needed to render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableOperationReceipt {
    pub operation: [u8; 16],
    pub payload_hash: [u8; 32],
    pub effect: Vec<u8>,
    pub frontier: ReplicaFrontier,
    pub bodies: Vec<BodyKey>,
    pub publication: crate::publication::PublicationId,
    pub transaction: [u8; 32],
}

/// Local readiness of the semantic publication named by a durable receipt.
/// Only `Ready` supplies an address that may be passed to `query_at`/`find_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationPublication {
    Ready(crate::publication::WorldPublicationId),
    Building,
    Capacity,
    ImplementationUnavailable,
    GenerationUnavailable,
    Unavailable,
}

/// Nonblocking readiness of a portable publication selected as the frozen
/// source for a composition-owned lifecycle plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleSourceStatus {
    Ready(LifecycleSourceCoordinate),
    Building,
    Capacity,
    ImplementationUnavailable,
    GenerationUnavailable,
    Unavailable,
}

/// Read-only reconciliation of one persistent idempotency coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStatus {
    Absent,
    Found {
        receipt: DurableOperationReceipt,
        publication: OperationPublication,
    },
}

enum ActionReceiptCheck {
    Replayed(replica::receipt::RequestReceipt),
    DurableAbsent(replica::ReceiptAbsence),
    ScratchAbsent,
}

/// Immutable Runtime publication state. The non-`Sync` Replica writer has its
/// own mutation mutex on [`StationCore`]; keeping it out of this state is what
/// lets Find, Live, and projection readers continue on the prior immutable
/// publication while a prepared candidate is extracted and serialized.
struct CoreInner {
    read_memory: Arc<ReadMemoryGovernor>,
    station_memory: Arc<StationMemoryLease>,
    /// Retained-cache share of the Station envelope. Production uses the full
    /// Station ceiling; tests may lower it to exercise eviction without
    /// allocating gigabytes of synthetic history.
    retained_cache_bytes_limit: u64,
    /// Optional device-local acceleration only. Opening or decoding this cache
    /// may never determine Replica/World truth availability.
    corpus_images: Option<Arc<crate::corpus_store::CorpusImageStore>>,
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
    publication_retention: Arc<PublicationRetentionLedger>,
    world_read_heads: std::collections::BTreeMap<
        (WorldId, crate::publication::WorldPublicationId),
        WorldReadHead,
    >,
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

/// Composition-owned physical read-memory envelope shared by every Station
/// opened from one Runtime. Per-Station ceilings prevent one Space from
/// monopolising the process; the process ceiling prevents N individually
/// valid Stations multiplying residency without bound.
pub(crate) struct ReadMemoryGovernor {
    process_bytes: u64,
    station_bytes: u64,
    next_station: std::sync::atomic::AtomicU64,
    state: std::sync::Mutex<std::collections::BTreeMap<u64, ReadMemoryAccount>>,
    /// Composition-owned bounded lane for exact historical publication work.
    /// Jobs already own a governor reservation before admission, while the
    /// bounded queue prevents many small historical receipts from creating an
    /// unbounded thread/task population across Stations.
    publication_jobs: std::sync::mpsc::SyncSender<PublicationJob>,
}

type PublicationJob = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, Clone, Copy, Default)]
struct ReadMemoryAccount {
    resident: u64,
    building: u64,
    analytical: u64,
    /// Lazily inflated canonical Body images retained by interactive readers.
    /// These bytes are separate from immutable publication metadata/corpora:
    /// eviction may release them without changing the selected publication.
    body_images: u64,
}

impl ReadMemoryAccount {
    fn total(self) -> u64 {
        self.resident
            .saturating_add(self.building)
            .saturating_add(self.analytical)
            .saturating_add(self.body_images)
    }
}

struct StationMemoryLease {
    governor: Arc<ReadMemoryGovernor>,
    station: u64,
}

struct BuildMemoryReservation {
    governor: std::sync::Weak<ReadMemoryGovernor>,
    station: u64,
    bytes: u64,
    active: bool,
}

struct ResidentMemoryTransition {
    governor: std::sync::Weak<ReadMemoryGovernor>,
    station: u64,
    prior_resident: u64,
    active: bool,
}

impl ResidentMemoryTransition {
    fn commit(mut self) {
        self.active = false;
    }
}

impl Drop for ResidentMemoryTransition {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Some(governor) = self.governor.upgrade() else {
            return;
        };
        if let Some(account) = governor.state.lock_recovering().get_mut(&self.station) {
            account.resident = self.prior_resident;
        };
    }
}

impl Drop for StationMemoryLease {
    fn drop(&mut self) {
        self.governor.state.lock_recovering().remove(&self.station);
    }
}

impl Drop for BuildMemoryReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Some(governor) = self.governor.upgrade() else {
            return;
        };
        if let Some(account) = governor.state.lock_recovering().get_mut(&self.station) {
            account.building = account.building.saturating_sub(self.bytes);
        };
    }
}

impl BuildMemoryReservation {
    /// Extend one in-flight reservation without creating an unaccounted gap
    /// between candidate snapshot materialization and Corpus admission.
    fn grow(&mut self, additional: u64) -> Result<(), ()> {
        if additional == 0 {
            return Ok(());
        }
        let governor = self.governor.upgrade().ok_or(())?;
        let mut state = governor.state.lock_recovering();
        let total = state
            .values()
            .fold(0u64, |sum, account| sum.saturating_add(account.total()));
        let account = state.get_mut(&self.station).ok_or(())?;
        if account.total().saturating_add(additional) > governor.station_bytes
            || total.saturating_add(additional) > governor.process_bytes
        {
            return Err(());
        }
        account.building = account.building.saturating_add(additional);
        self.bytes = self.bytes.saturating_add(additional);
        Ok(())
    }

    /// Atomically convert transient build capacity into the Station's new
    /// immutable resident footprint. The caller performs the infallible map
    /// installation immediately after this succeeds while it still holds the
    /// Station writer.
    fn prepare_resident(mut self, resident: u64) -> Result<ResidentMemoryTransition, ()> {
        let governor = self.governor.upgrade().ok_or(())?;
        let mut state = governor.state.lock_recovering();
        let current = state.get(&self.station).copied().ok_or(())?;
        let other = state
            .iter()
            .filter(|(candidate, _)| **candidate != self.station)
            .fold(0u64, |bytes, (_, account)| {
                bytes.saturating_add(account.total())
            });
        let remaining_building = current.building.saturating_sub(self.bytes);
        if resident
            .saturating_add(remaining_building)
            .saturating_add(current.analytical)
            .saturating_add(current.body_images)
            > governor.station_bytes
            || other
                .saturating_add(resident)
                .saturating_add(remaining_building)
                .saturating_add(current.analytical)
                .saturating_add(current.body_images)
                > governor.process_bytes
        {
            return Err(());
        }
        let account = state.get_mut(&self.station).ok_or(())?;
        account.building = remaining_building;
        account.resident = resident;
        self.active = false;
        Ok(ResidentMemoryTransition {
            governor: Arc::downgrade(&governor),
            station: self.station,
            prior_resident: current.resident,
            active: true,
        })
    }

    fn finish(self, resident: u64) -> Result<(), ()> {
        self.prepare_resident(resident).map(|transition| {
            transition.commit();
        })
    }
}

/// Product analytical work admitted against the same physical envelope as its
/// source publication. The transient reservation exists before a worker is
/// queued; successful compilation converts it into a retained lease owned by
/// the exact artifact cache entry.
pub(crate) struct AnalyticalBuildReservation {
    build: BuildMemoryReservation,
    station: Arc<StationMemoryLease>,
}

pub(crate) struct AnalyticalRetainedLease {
    station: Arc<StationMemoryLease>,
    bytes: u64,
}

/// One interactive Body-image inflation admitted before any protected bytes
/// are read or decrypted. Successful loads convert this transient reservation
/// into a retained lease owned by the cache image; cancellation and every
/// typed resolver failure release it through `Drop`.
pub(crate) struct BodyImageBuildReservation {
    build: BuildMemoryReservation,
    station: Arc<StationMemoryLease>,
}

pub(crate) struct BodyImageRetainedLease {
    station: Arc<StationMemoryLease>,
    bytes: u64,
}

struct StationBodyImageMemory {
    read_memory: Arc<ReadMemoryGovernor>,
    station: Arc<StationMemoryLease>,
}

struct StationBodyImageReservation(Option<BodyImageBuildReservation>);

struct StationBodyImageLease {
    _lease: BodyImageRetainedLease,
}

impl crate::body_image::BodyImageMemory for StationBodyImageMemory {
    fn reserve(
        &self,
        transient_bytes: u64,
    ) -> Result<
        Box<dyn crate::body_image::BodyImageMemoryReservation>,
        crate::body_image::BodyImageFailure,
    > {
        self.read_memory
            .reserve_body_image(self.station.clone(), transient_bytes)
            .map(|reservation| {
                Box::new(StationBodyImageReservation(Some(reservation)))
                    as Box<dyn crate::body_image::BodyImageMemoryReservation>
            })
            .map_err(|_| crate::body_image::BodyImageFailure::Capacity)
    }
}

impl crate::body_image::BodyImageMemoryReservation for StationBodyImageReservation {
    fn retain(
        mut self: Box<Self>,
        retained_bytes: u64,
    ) -> Result<Box<dyn crate::body_image::BodyImageMemoryLease>, crate::body_image::BodyImageFailure>
    {
        self.0
            .take()
            .ok_or(crate::body_image::BodyImageFailure::Interrupted)?
            .retain(retained_bytes)
            .map(|lease| {
                Box::new(StationBodyImageLease { _lease: lease })
                    as Box<dyn crate::body_image::BodyImageMemoryLease>
            })
            .map_err(|_| crate::body_image::BodyImageFailure::Capacity)
    }
}

impl crate::body_image::BodyImageMemoryLease for StationBodyImageLease {}

impl AnalyticalBuildReservation {
    pub(crate) fn retain(mut self, bytes: u64) -> Result<AnalyticalRetainedLease, ()> {
        let governor = &self.station.governor;
        let mut state = governor.state.lock_recovering();
        let current = state.get(&self.station.station).copied().ok_or(())?;
        let other = state
            .iter()
            .filter(|(candidate, _)| **candidate != self.station.station)
            .fold(0u64, |total, (_, account)| {
                total.saturating_add(account.total())
            });
        let remaining_building = current.building.saturating_sub(self.build.bytes);
        let analytical = current.analytical.saturating_add(bytes);
        let station_total = current
            .resident
            .saturating_add(remaining_building)
            .saturating_add(analytical)
            .saturating_add(current.body_images);
        if station_total > governor.station_bytes
            || other.saturating_add(station_total) > governor.process_bytes
        {
            return Err(());
        }
        let account = state.get_mut(&self.station.station).ok_or(())?;
        account.building = remaining_building;
        account.analytical = analytical;
        self.build.active = false;
        let station = self.station.clone();
        drop(state);
        Ok(AnalyticalRetainedLease { station, bytes })
    }
}

impl BodyImageBuildReservation {
    pub(crate) fn retain(mut self, bytes: u64) -> Result<BodyImageRetainedLease, ()> {
        let governor = &self.station.governor;
        let mut state = governor.state.lock_recovering();
        let current = state.get(&self.station.station).copied().ok_or(())?;
        let other = state
            .iter()
            .filter(|(candidate, _)| **candidate != self.station.station)
            .fold(0u64, |total, (_, account)| {
                total.saturating_add(account.total())
            });
        let remaining_building = current.building.saturating_sub(self.build.bytes);
        let body_images = current.body_images.saturating_add(bytes);
        let station_total = current
            .resident
            .saturating_add(remaining_building)
            .saturating_add(current.analytical)
            .saturating_add(body_images);
        if station_total > governor.station_bytes
            || other.saturating_add(station_total) > governor.process_bytes
        {
            return Err(());
        }
        let account = state.get_mut(&self.station.station).ok_or(())?;
        account.building = remaining_building;
        account.body_images = body_images;
        self.build.active = false;
        let station = self.station.clone();
        drop(state);
        Ok(BodyImageRetainedLease { station, bytes })
    }
}

impl Drop for BodyImageRetainedLease {
    fn drop(&mut self) {
        if let Some(account) = self
            .station
            .governor
            .state
            .lock_recovering()
            .get_mut(&self.station.station)
        {
            account.body_images = account.body_images.saturating_sub(self.bytes);
        };
    }
}

impl Drop for AnalyticalRetainedLease {
    fn drop(&mut self) {
        if let Some(account) = self
            .station
            .governor
            .state
            .lock_recovering()
            .get_mut(&self.station.station)
        {
            account.analytical = account.analytical.saturating_sub(self.bytes);
        };
    }
}

impl ReadMemoryGovernor {
    pub(crate) fn process_default() -> Arc<Self> {
        Arc::new(Self::with_limits(
            4 * 1024 * 1024 * 1024,
            MAX_STATION_READ_RETAINED_BYTES,
        ))
    }

    #[cfg(test)]
    pub(crate) fn with_test_limits(process_bytes: u64, station_bytes: u64) -> Arc<Self> {
        Arc::new(Self::with_limits(process_bytes, station_bytes))
    }

    fn with_limits(process_bytes: u64, station_bytes: u64) -> Self {
        let (publication_jobs, receiver) = std::sync::mpsc::sync_channel::<PublicationJob>(8);
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(2);
        for index in 0..workers {
            let receiver = receiver.clone();
            let _ = std::thread::Builder::new()
                .name(format!("lait-publication-{index}"))
                .spawn(move || loop {
                    let job = receiver.lock_recovering().recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => break,
                    }
                });
        }
        Self {
            process_bytes,
            station_bytes: station_bytes.min(process_bytes),
            next_station: std::sync::atomic::AtomicU64::new(1),
            state: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            publication_jobs,
        }
    }

    fn schedule_publication(&self, job: PublicationJob) -> Result<(), PublicationJob> {
        self.publication_jobs
            .try_send(job)
            .map_err(|failure| match failure {
                std::sync::mpsc::TrySendError::Full(job)
                | std::sync::mpsc::TrySendError::Disconnected(job) => job,
            })
    }

    fn register(self: &Arc<Self>, resident: u64) -> Result<Arc<StationMemoryLease>, ()> {
        let station = self
            .next_station
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.state.lock_recovering();
        let total = state
            .values()
            .fold(0u64, |bytes, account| bytes.saturating_add(account.total()));
        if resident > self.station_bytes || total.saturating_add(resident) > self.process_bytes {
            return Err(());
        }
        state.insert(
            station,
            ReadMemoryAccount {
                resident,
                building: 0,
                analytical: 0,
                body_images: 0,
            },
        );
        Ok(Arc::new(StationMemoryLease {
            governor: self.clone(),
            station,
        }))
    }

    fn set_resident(&self, station: u64, resident: u64) -> Result<(), ()> {
        let mut state = self.state.lock_recovering();
        let current = state.get(&station).copied().ok_or(())?;
        let other = state
            .iter()
            .filter(|(candidate, _)| **candidate != station)
            .fold(0u64, |bytes, (_, account)| {
                bytes.saturating_add(account.total())
            });
        if resident
            .saturating_add(current.building)
            .saturating_add(current.analytical)
            .saturating_add(current.body_images)
            > self.station_bytes
            || other
                .saturating_add(resident)
                .saturating_add(current.building)
                .saturating_add(current.analytical)
                .saturating_add(current.body_images)
                > self.process_bytes
        {
            return Err(());
        }
        state.get_mut(&station).ok_or(())?.resident = resident;
        Ok(())
    }

    fn reserve_build(
        self: &Arc<Self>,
        station: u64,
        bytes: u64,
    ) -> Result<BuildMemoryReservation, ()> {
        let mut state = self.state.lock_recovering();
        let total = state
            .values()
            .fold(0u64, |sum, account| sum.saturating_add(account.total()));
        let account = state.get_mut(&station).ok_or(())?;
        if account.total().saturating_add(bytes) > self.station_bytes
            || total.saturating_add(bytes) > self.process_bytes
        {
            return Err(());
        }
        account.building = account.building.saturating_add(bytes);
        Ok(BuildMemoryReservation {
            governor: Arc::downgrade(self),
            station,
            bytes,
            active: true,
        })
    }

    fn reserve_body_image(
        self: &Arc<Self>,
        station: Arc<StationMemoryLease>,
        bytes: u64,
    ) -> Result<BodyImageBuildReservation, ()> {
        let build = self.reserve_build(station.station, bytes)?;
        Ok(BodyImageBuildReservation { build, station })
    }
}

struct CursorLease {
    publication: Arc<WorldPublication>,
    expires: std::time::Instant,
}

struct DeferredLeaseState {
    publication: Arc<WorldPublication>,
    holders: usize,
}

#[derive(Default)]
struct PublicationRetentionLedger {
    deferred: std::sync::Mutex<
        std::collections::BTreeMap<
            (WorldId, crate::publication::WorldPublicationId),
            DeferredLeaseState,
        >,
    >,
}

struct DeferredPublicationLease {
    retention: std::sync::Weak<PublicationRetentionLedger>,
    key: (WorldId, crate::publication::WorldPublicationId),
    _station_memory: Arc<StationMemoryLease>,
}

impl crate::world::FindLease for DeferredPublicationLease {}

impl Drop for DeferredPublicationLease {
    fn drop(&mut self) {
        let Some(retention) = self.retention.upgrade() else {
            return;
        };
        retention.release(&self.key);
    }
}

impl PublicationRetentionLedger {
    /// Pin an exact publication that the callback already owns in the Station
    /// read image. `admitted_retained_bytes` was measured while the Station
    /// writer selected that image; adding the first pin therefore adds no
    /// bytes immediately, but makes those bytes authoritative when later cache
    /// trimming would otherwise discard the publication.
    ///
    /// This ledger deliberately has its own mutex. World callbacks execute
    /// while the Station writer is held, so acquiring or dropping a detached
    /// Find handle must never attempt to re-enter `StationCore`.
    fn acquire_existing(
        self: &Arc<Self>,
        key: (WorldId, crate::publication::WorldPublicationId),
        publication: Arc<WorldPublication>,
        admitted_retained_bytes: u64,
        station_memory: Arc<StationMemoryLease>,
    ) -> Result<Arc<DeferredPublicationLease>, ()> {
        let mut deferred = self.deferred.lock_recovering();
        if let Some(lease) = deferred.get_mut(&key) {
            lease.holders = lease.holders.saturating_add(1);
        } else {
            if admitted_retained_bytes > MAX_STATION_READ_RETAINED_BYTES {
                return Err(());
            }
            deferred.insert(
                key.clone(),
                DeferredLeaseState {
                    publication,
                    holders: 1,
                },
            );
        }
        Ok(Arc::new(DeferredPublicationLease {
            retention: Arc::downgrade(self),
            key,
            _station_memory: station_memory,
        }))
    }

    fn release(&self, key: &(WorldId, crate::publication::WorldPublicationId)) {
        let mut deferred = self.deferred.lock_recovering();
        let remove = deferred.get_mut(key).is_some_and(|lease| {
            lease.holders = lease.holders.saturating_sub(1);
            lease.holders == 0
        });
        if remove {
            deferred.remove(key);
        }
    }

    fn contains(&self, key: &(WorldId, crate::publication::WorldPublicationId)) -> bool {
        self.deferred.lock_recovering().contains_key(key)
    }

    fn publication(
        &self,
        key: &(WorldId, crate::publication::WorldPublicationId),
    ) -> Option<Arc<WorldPublication>> {
        self.deferred
            .lock_recovering()
            .get(key)
            .map(|lease| lease.publication.clone())
    }

    fn publications(&self) -> Vec<Arc<WorldPublication>> {
        self.deferred
            .lock_recovering()
            .values()
            .map(|lease| lease.publication.clone())
            .collect()
    }
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
    read_memory: Arc<ReadMemoryGovernor>,
    station_memory: Arc<StationMemoryLease>,
    publication_retention: Arc<PublicationRetentionLedger>,
    admitted_retained_bytes: u64,
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

    fn acquire_deferred(&self) -> Result<Arc<dyn crate::world::FindLease>, crate::find::Failure> {
        let key = (self.world.clone(), self.publication.id);
        self.publication_retention
            .acquire_existing(
                key,
                self.publication.clone(),
                self.admitted_retained_bytes,
                self.station_memory.clone(),
            )
            .map(|lease| lease as Arc<dyn crate::world::FindLease>)
            .map_err(|_| crate::find::Failure::CursorCapacityExceeded)
    }

    fn reserve_analysis(
        &self,
        transient_bytes: u64,
    ) -> Result<crate::world::AnalyticalMemoryReservation, crate::find::Failure> {
        let build = self
            .read_memory
            .reserve_build(self.station_memory.station, transient_bytes)
            .map_err(|_| crate::find::Failure::CursorCapacityExceeded)?;
        Ok(crate::world::AnalyticalMemoryReservation::new(
            AnalyticalBuildReservation {
                build,
                station: self.station_memory.clone(),
            },
        ))
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublicationFailure {
    Interrupted,
    Capacity,
    Generation,
    Extractor {
        source: Option<crate::find::SourceRef>,
        body: Option<BodyKey>,
        stage: &'static str,
        /// The bounded typed refusal returned by the World callback. Keeping
        /// it on the exact read head makes a repeated query diagnosable even
        /// after the original tracing event has rolled out of the log buffer.
        /// Non-callback contract stages and callback panics have no refusal.
        rejection: Option<crate::world::Rejection>,
    },
    Corpus,
}

impl PublicationFailure {
    /// Whether retrying the same exact publication can succeed without a new
    /// durable generation, package activation, or key-capability snapshot.
    ///
    /// Retryable failures must not become terminal `WorldReadHead` entries:
    /// doing so would turn one transient admission refusal or interrupted
    /// worker into permanent unavailability at otherwise valid coordinates.
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Interrupted | Self::Capacity)
            || matches!(
                self,
                Self::Extractor {
                    rejection: Some(Rejection::BodyRead(
                        crate::world::BodyReadFailure::Capacity(_)
                            | crate::world::BodyReadFailure::Interrupted(_)
                    )),
                    ..
                }
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorldReadHead {
    Building,
    Ready,
    Unavailable(PublicationFailure),
}

fn extractor_publication_failure(
    extractor: Option<&crate::find::Extractor>,
    body: Option<&BodyKey>,
    stage: &'static str,
) -> PublicationFailure {
    PublicationFailure::Extractor {
        source: extractor.map(|extractor| extractor.source.clone()),
        body: body.cloned(),
        stage,
        rejection: None,
    }
}

fn extractor_rejection_publication_failure(
    extractor: &crate::find::Extractor,
    body: &BodyKey,
    rejection: crate::world::Rejection,
) -> PublicationFailure {
    PublicationFailure::Extractor {
        source: Some(extractor.source.clone()),
        body: Some(body.clone()),
        stage: "callback-rejection",
        rejection: Some(rejection),
    }
}

fn record_world_read_failure(
    inner: &mut CoreInner,
    key: (WorldId, crate::publication::WorldPublicationId),
    failure: PublicationFailure,
) {
    if failure.is_retryable() {
        inner.world_read_heads.remove(&key);
    } else {
        inner
            .world_read_heads
            .insert(key, WorldReadHead::Unavailable(failure));
    }
}

#[derive(Clone)]
struct CachedReadGeneration {
    snapshot: Arc<replica::ReadSnapshot>,
    materialization: crate::publication::MaterializationId,
}

/// Active cursor continuations are leases, not hints. This count bounds cursor
/// metadata and abuse; the composition-owned byte governor independently
/// bounds every uniquely retained publication. A full table refuses a new
/// cursor rather than evicting one another request can still present.
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
const MAX_STATION_READ_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[cfg(test)]
mod read_memory_governor_tests {
    use super::{AnalyticalBuildReservation, ReadMemoryGovernor};

    #[test]
    fn two_stations_share_one_process_envelope() {
        let governor = ReadMemoryGovernor::with_test_limits(100, 80);
        let first = governor.register(60).expect("first Station fits");
        assert!(governor.register(50).is_err());
        let second = governor.register(40).expect("remaining process bytes fit");
        drop(first);
        let replacement = governor
            .register(60)
            .expect("dropping a Station returns its physical reservation");
        drop((second, replacement));
    }

    #[test]
    fn concurrent_distinct_builds_reserve_before_allocation() {
        let governor = ReadMemoryGovernor::with_test_limits(220, 180);
        let first = governor.register(80).expect("first baseline");
        let second = governor.register(80).expect("second baseline");
        let flight = governor
            .reserve_build(first.station, 50)
            .expect("one large historical flight fits");
        assert!(governor.reserve_build(second.station, 20).is_err());
        drop(flight);
        assert!(governor.reserve_build(second.station, 20).is_ok());
    }

    #[test]
    fn failed_durability_rolls_back_prepared_residency() {
        let governor = ReadMemoryGovernor::with_test_limits(200, 200);
        let station = governor.register(50).expect("baseline");
        let build = governor
            .reserve_build(station.station, 100)
            .expect("candidate peak");
        let transition = build.prepare_resident(120).expect("candidate steady state");
        assert_eq!(
            governor
                .state
                .lock()
                .expect("governor lock")
                .get(&station.station)
                .expect("account")
                .resident,
            120
        );
        drop(transition);
        assert_eq!(
            governor
                .state
                .lock()
                .expect("governor lock")
                .get(&station.station)
                .expect("account")
                .resident,
            50
        );
    }

    #[test]
    fn analytical_work_shares_capacity_and_releases_retention() {
        let governor = ReadMemoryGovernor::with_test_limits(100, 100);
        let station = governor.register(90).expect("source publication");
        assert!(
            governor.reserve_build(station.station, 11).is_err(),
            "analytical projection is refused before its worker can allocate"
        );

        let build = governor
            .reserve_build(station.station, 10)
            .expect("remaining analytical working memory");
        let retained = AnalyticalBuildReservation {
            build,
            station: station.clone(),
        }
        .retain(5)
        .expect("working reservation converts to artifact retention");
        let account = governor
            .state
            .lock()
            .expect("governor lock")
            .get(&station.station)
            .copied()
            .expect("account");
        assert_eq!(account.resident, 90);
        assert_eq!(account.building, 0);
        assert_eq!(account.analytical, 5);

        drop(retained);
        assert_eq!(
            governor
                .state
                .lock()
                .expect("governor lock")
                .get(&station.station)
                .expect("account")
                .analytical,
            0
        );
    }

    #[test]
    fn interactive_body_images_share_capacity_and_release_after_last_pin() {
        let governor = ReadMemoryGovernor::with_test_limits(100, 100);
        let station = governor.register(40).expect("publication metadata");
        let build = governor
            .reserve_body_image(station.clone(), 50)
            .expect("bounded protected/decode working set");
        let retained = build
            .retain(30)
            .expect("one exact image replaces transient capacity");
        assert!(governor.reserve_body_image(station.clone(), 31).is_err());
        let account = governor
            .state
            .lock()
            .expect("governor lock")
            .get(&station.station)
            .copied()
            .expect("account");
        assert_eq!(account.resident, 40);
        assert_eq!(account.building, 0);
        assert_eq!(account.body_images, 30);

        drop(retained);
        assert!(governor.reserve_body_image(station, 60).is_ok());
    }
}

impl CoreInner {
    fn sync_read_memory(&self) -> Result<(), ()> {
        self.read_memory.set_resident(
            self.station_memory.station,
            self.station_read_retained_bytes(),
        )
    }

    fn reserve_build_memory(
        &mut self,
        snapshot_bytes: u64,
        corpus: crate::corpus::BuildMemory,
    ) -> Result<BuildMemoryReservation, ()> {
        let bytes = snapshot_bytes
            .saturating_add(corpus.retained_bytes)
            .saturating_add(corpus.transient_bytes);
        self.make_read_room(bytes)?;
        self.read_memory
            .reserve_build(self.station_memory.station, bytes)
    }

    fn reserve_full_publication_build(
        &mut self,
        snapshot: &replica::ReadSnapshot,
        world: &WorldId,
        extractors: &[crate::find::Extractor],
        snapshot_already_resident: bool,
    ) -> Result<BuildMemoryReservation, ()> {
        let corpus = crate::corpus::Corpus::estimate_build_bytes(snapshot, world, extractors);
        let snapshot_bytes = if snapshot_already_resident {
            0
        } else {
            snapshot.retained_bytes_estimate()
        };
        self.reserve_build_memory(snapshot_bytes, corpus)
    }

    fn reserve_historical_publication_build(
        &mut self,
        footprint: &replica::GenerationFootprint,
        world: &WorldId,
        extractors: &[crate::find::Extractor],
    ) -> Result<BuildMemoryReservation, ()> {
        let mut corpus = crate::corpus::Corpus::estimate_build_bytes_from_footprint(
            footprint, world, extractors,
        );
        corpus.transient_bytes = corpus
            .transient_bytes
            .saturating_add(footprint.reconstruction_transient_bytes);
        self.reserve_build_memory(footprint.snapshot_retained_bytes, corpus)
    }

    fn finish_publication_build(
        &self,
        reservation: BuildMemoryReservation,
        publication: &Arc<WorldPublication>,
    ) -> Result<(), ()> {
        let resident = self.publication_resident_bytes(publication);
        reservation.finish(resident)
    }

    fn publication_resident_bytes(&self, publication: &Arc<WorldPublication>) -> u64 {
        self.station_read_retained_bytes()
            .saturating_add(self.publication_incremental_bytes(publication))
    }

    fn station_read_retained_bytes(&self) -> u64 {
        let mut snapshots = std::collections::BTreeSet::<usize>::new();
        let mut corpora = std::collections::BTreeSet::<usize>::new();
        let mut bytes = 0u64;
        let mut add_snapshot = |snapshot: &Arc<replica::ReadSnapshot>| {
            let identity = Arc::as_ptr(snapshot) as usize;
            if snapshots.insert(identity) {
                bytes = bytes.saturating_add(snapshot.retained_bytes_estimate());
            }
        };
        for generation in self.generations.values() {
            add_snapshot(&generation.snapshot);
        }
        drop(add_snapshot);
        let mut add_publication = |publication: &Arc<WorldPublication>| {
            let snapshot = Arc::as_ptr(&publication.snapshot) as usize;
            if snapshots.insert(snapshot) {
                bytes = bytes.saturating_add(publication.snapshot.retained_bytes_estimate());
            }
            let corpus = Arc::as_ptr(&publication.corpus) as usize;
            if corpora.insert(corpus) {
                bytes = bytes.saturating_add(publication.corpus.retained_bytes_estimate());
            }
        };
        for publication in self.world_publications.values() {
            add_publication(publication);
        }
        for publication in self.retained_world_publications.values() {
            add_publication(publication);
        }
        for lease in self.cursor_leases.values() {
            add_publication(&lease.publication);
        }
        for publication in self.publication_retention.publications() {
            add_publication(&publication);
        }
        bytes
    }

    fn publication_incremental_bytes(&self, publication: &Arc<WorldPublication>) -> u64 {
        let snapshot_known = self
            .generations
            .values()
            .any(|generation| Arc::ptr_eq(&generation.snapshot, &publication.snapshot))
            || self
                .world_publications
                .values()
                .any(|candidate| Arc::ptr_eq(&candidate.snapshot, &publication.snapshot))
            || self
                .retained_world_publications
                .values()
                .any(|candidate| Arc::ptr_eq(&candidate.snapshot, &publication.snapshot))
            || self
                .cursor_leases
                .values()
                .any(|lease| Arc::ptr_eq(&lease.publication.snapshot, &publication.snapshot))
            || self
                .publication_retention
                .publications()
                .iter()
                .any(|candidate| Arc::ptr_eq(&candidate.snapshot, &publication.snapshot));
        let corpus_known = self
            .world_publications
            .values()
            .any(|candidate| Arc::ptr_eq(&candidate.corpus, &publication.corpus))
            || self
                .retained_world_publications
                .values()
                .any(|candidate| Arc::ptr_eq(&candidate.corpus, &publication.corpus))
            || self
                .cursor_leases
                .values()
                .any(|lease| Arc::ptr_eq(&lease.publication.corpus, &publication.corpus))
            || self
                .publication_retention
                .publications()
                .iter()
                .any(|candidate| Arc::ptr_eq(&candidate.corpus, &publication.corpus));
        (!snapshot_known)
            .then(|| publication.snapshot.retained_bytes_estimate())
            .unwrap_or(0)
            .saturating_add(
                (!corpus_known)
                    .then(|| publication.corpus.retained_bytes_estimate())
                    .unwrap_or(0),
            )
    }

    fn publication_fits(&self, publication: &Arc<WorldPublication>) -> bool {
        self.station_read_retained_bytes()
            .saturating_add(self.publication_incremental_bytes(publication))
            <= self.retained_cache_bytes_limit
    }

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
        self.trim_generation_cache();
    }

    fn trim_generation_cache(&mut self) {
        self.evict_unpinned_read_cache_to(self.retained_cache_bytes_limit);
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

    /// Resolve one complete Station-local publication coordinate without
    /// falling back to another materialization of the same semantic root.
    ///
    /// Current, cache-retained, cursor-leased, and deferred-analysis Arcs all
    /// count as retained. The returned Arc pins the exact image for the full
    /// callback/evaluation even if cache trimming runs concurrently.
    fn exact_world_publication(
        &mut self,
        key: &(WorldId, crate::publication::WorldPublicationId),
    ) -> Option<Arc<WorldPublication>> {
        self.purge_cursor_leases();
        self.world_publications
            .get(&key.0)
            .filter(|publication| publication.id == key.1)
            .cloned()
            .or_else(|| self.retained_world_publications.get(key).cloned())
            .or_else(|| {
                self.cursor_leases
                    .get(key)
                    .map(|lease| lease.publication.clone())
            })
            .or_else(|| self.publication_retention.publication(key))
    }

    fn lease_world_publication(
        &mut self,
        world: WorldId,
        publication: Arc<WorldPublication>,
    ) -> Result<(), ()> {
        self.purge_cursor_leases();
        let key = (world, publication.id);
        if let Some(lease) = self.cursor_leases.get_mut(&key) {
            lease.expires = std::time::Instant::now() + CURSOR_LEASE_TTL;
            lease.publication = publication;
            return Ok(());
        }
        if self.cursor_leases.len() == MAX_CURSOR_LEASES {
            return Err(());
        }
        if !self.publication_fits(&publication) {
            return Err(());
        }
        self.cursor_leases.insert(
            key,
            CursorLease {
                publication,
                expires: std::time::Instant::now() + CURSOR_LEASE_TTL,
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
        self.evict_unpinned_read_cache_to(self.retained_cache_bytes_limit);
    }

    /// Drop acceleration-only generations/publications until `limit` can hold
    /// the remaining immutable read set. Current heads, active cursors, and
    /// deferred analytical handles are never selected: they are authoritative
    /// reservations, while these maps are reconstructable caches.
    fn evict_unpinned_read_cache_to(&mut self, limit: u64) {
        while self.station_read_retained_bytes() > limit {
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
                let deferred = self.publication_retention.contains(&candidate);
                if current || leased || deferred {
                    self.world_publication_order.push_back(candidate);
                } else {
                    let root = candidate.1.publication.manifest_root;
                    self.retained_world_publications.remove(&candidate);
                    let generation_pinned =
                        root == self.snapshot.root()
                            || self.world_publications.values().any(|publication| {
                                publication.id.publication.manifest_root == root
                            })
                            || self.cursor_leases.values().any(|lease| {
                                lease.publication.id.publication.manifest_root == root
                            })
                            || self.publication_retention.publications().iter().any(
                                |publication| publication.id.publication.manifest_root == root,
                            );
                    if !generation_pinned {
                        self.generations.remove(&root);
                        self.parents.remove(&root);
                        self.generation_order.retain(|candidate| candidate != &root);
                    }
                    removed = true;
                    break;
                }
            }
            if removed {
                continue;
            }

            let attempts = self.generation_order.len();
            for _ in 0..attempts {
                let Some(candidate) = self.generation_order.pop_front() else {
                    break;
                };
                if candidate == self.snapshot.root() {
                    self.generation_order.push_back(candidate);
                    continue;
                }
                self.generations.remove(&candidate);
                self.parents.remove(&candidate);
                removed = true;
                break;
            }
            if !removed {
                break;
            }
        }
    }

    /// Create physical headroom before a build reservation is admitted. Cache
    /// eviction must happen *before* allocating a candidate snapshot/corpus;
    /// otherwise a Station at its retained limit can no longer publish even a
    /// tiny delta despite having gigabytes of unpinned historical cache.
    fn make_read_room(&mut self, additional: u64) -> Result<(), ()> {
        let limit = self.retained_cache_bytes_limit.saturating_sub(additional);
        self.evict_unpinned_read_cache_to(limit);
        if self.station_read_retained_bytes() > limit {
            return Err(());
        }
        self.sync_read_memory()
    }

    /// Create physical governor headroom for bounded transient work which is
    /// not itself a retained publication. A deliberately lower retained-cache
    /// ceiling still constrains publication builds, but must not prevent a
    /// caller from reading the durable receipt that reports that capacity.
    fn make_transient_read_room(&mut self, additional: u64) -> Result<(), ()> {
        let limit = self.read_memory.station_bytes.saturating_sub(additional);
        self.evict_unpinned_read_cache_to(limit);
        if self.station_read_retained_bytes() > limit {
            return Err(());
        }
        self.sync_read_memory()
    }

    fn install_world_publication(&mut self, world: WorldId, publication: Arc<WorldPublication>) {
        self.world_read_heads
            .insert((world.clone(), publication.id), WorldReadHead::Ready);
        self.world_publications
            .insert(world.clone(), publication.clone());
        self.retain_world_publication(world, publication);
        if self.sync_read_memory().is_err() {
            tracing::error!("Station read-memory envelope exceeded while installing publication");
        }
    }

    fn ready_semantic_publication(
        &self,
        world: &WorldId,
        semantic: crate::publication::PublicationId,
    ) -> Option<Arc<WorldPublication>> {
        self.world_publications
            .get(world)
            .filter(|publication| publication.id.publication == semantic)
            .cloned()
            .or_else(|| {
                self.retained_world_publications
                    .iter()
                    .find(|((candidate_world, id), _)| {
                        candidate_world == world && id.publication == semantic
                    })
                    .map(|(_, publication)| publication.clone())
            })
            .or_else(|| {
                self.cursor_leases
                    .iter()
                    .find(|((candidate_world, id), _)| {
                        candidate_world == world && id.publication == semantic
                    })
                    .map(|(_, lease)| lease.publication.clone())
            })
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
                match publication.corpus.apply(crate::corpus::CorpusDelta {
                    base: publication.id,
                    next,
                    // The Corpus has no changed source rows for this World,
                    // but its publication coordinate is the new global
                    // Replica generation. Supplying the prior snapshot makes
                    // the exact-root validation reject this carry-forward and
                    // leaves an unaffected World permanently `Building`.
                    snapshot: snapshot.clone(),
                    bodies: Vec::new(),
                }) {
                    Ok((corpus, _)) => self.install_world_publication(
                        world,
                        Arc::new(WorldPublication {
                            id: next,
                            snapshot: snapshot.clone(),
                            corpus: Arc::new(corpus),
                        }),
                    ),
                    Err(_) => {
                        record_world_read_failure(self, (world, next), PublicationFailure::Corpus)
                    }
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
    /// The unit is one signed semantic transaction, not a transport or
    /// durability phase. A Contact bundle publishes one record per contributing
    /// transaction so actors are never collapsed; authority advancement rides
    /// the first record or stands alone. Scopes may span Worlds.
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

/// A Station's durable mutation lane and immutable publication directory.
/// Shared by the Station and every Session; a Session can commit through it
/// but never stop the Station.
pub struct StationCore {
    /// Bounded admission for the one durable mutation lane. Callers use
    /// `try_lock`: a competing intent receives a prompt typed refusal instead
    /// of waiting invisibly while another operation validates or extracts.
    mutation_lane: std::sync::Mutex<()>,
    /// The durable writer itself. An owned PreparedAction lets Runtime release
    /// this lock during snapshot projection and Corpus construction, then
    /// reacquire it for exact-parent CAS/finalize.
    replica: std::sync::Mutex<replica::Replica>,
    /// Short-lived state/install mutex. Lock order is mechanically one-way:
    /// code that needs both acquires `replica` before `inner`; code holding
    /// `inner` must never attempt to acquire `replica`. Query and Find readers
    /// pin an `Arc<WorldPublication>` here and then evaluate without either.
    inner: std::sync::Mutex<CoreInner>,
    /// Mutable interactive acceleration outside the immutable publication
    /// atom. Every entry is material-identity keyed and governor-accounted;
    /// Corpus extraction bypasses it so a full scan cannot evict hot Bodies.
    body_images: Arc<crate::body_image::BodyImageCache>,
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
    #[cfg(test)]
    pub fn for_test(replica: replica::Replica) -> Self {
        match Self::new(
            Epoch::ZERO,
            DEFAULT_OBSERVATION_CAPACITY,
            replica,
            ReadMemoryGovernor::process_default(),
            None,
        ) {
            Ok(core) => core,
            Err(()) => panic!("empty test Station exceeds the read-memory envelope"),
        }
    }

    pub(crate) fn new(
        epoch: Epoch,
        observation_capacity: usize,
        replica: replica::Replica,
        read_memory: Arc<ReadMemoryGovernor>,
        corpus_images: Option<Arc<crate::corpus_store::CorpusImageStore>>,
    ) -> Result<Self, ()> {
        let frontier = replica.frontier();
        let snapshot = Arc::new(replica.read_snapshot());
        let root = snapshot.root();
        let station_memory = read_memory.register(snapshot.retained_bytes_estimate())?;
        let body_images = Arc::new(crate::body_image::BodyImageCache::new(
            Arc::new(StationBodyImageMemory {
                read_memory: read_memory.clone(),
                station: station_memory.clone(),
            }),
            crate::body_image::MAX_HOT_BODY_IMAGES,
        ));
        let retained_cache_bytes_limit = read_memory.station_bytes;
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
        Ok(Self {
            mutation_lane: std::sync::Mutex::new(()),
            replica: std::sync::Mutex::new(replica),
            inner: std::sync::Mutex::new(CoreInner {
                read_memory,
                station_memory,
                retained_cache_bytes_limit,
                corpus_images,
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
                publication_retention: Arc::new(PublicationRetentionLedger::default()),
                world_read_heads: std::collections::BTreeMap::new(),
                publication_flights: std::collections::BTreeMap::new(),
                world_builders: std::collections::BTreeMap::new(),
                offers: std::collections::BTreeMap::new(),
                challenges: std::collections::BTreeMap::new(),
                readies: std::collections::BTreeMap::new(),
                closed: false,
            }),
            body_images,
            broadcaster: Arc::new(Broadcaster::new(epoch, observation_capacity, frontier)),
            authority_tick: tokio::sync::watch::Sender::new(0),
            exec: std::sync::Mutex::new(ExecGate {
                inflight: std::collections::BTreeSet::new(),
                busy: false,
            }),
            exec_tick: tokio::sync::watch::Sender::new(0),
        })
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
    pub fn note_authority_advanced(self: &Arc<Self>) {
        // Authority/key arrival can change which retained Bodies are readable
        // without moving the Manifest. No old corpus remains addressable under
        // that same semantic root. Install a fresh materialization as Building
        // and schedule its rebuild on the bounded publication lane before
        // returning; callers receive immediate feedback while the prior exact
        // view stays retained and no write can overtake newly readable truth.
        let rebuilds = {
            let _replica = self.replica_lock();
            let mut inner = self.lock();
            let snapshot = inner.snapshot.clone();
            let parent = inner.parents.get(&snapshot.root()).copied().flatten();
            inner.publish_snapshot(snapshot, parent, None);
            inner
                .world_builders
                .iter()
                .map(|(world, builder)| {
                    (
                        world.clone(),
                        crate::publication::PublicationId::new(
                            inner.snapshot.root(),
                            builder.implementation,
                            builder.extractor_schema_digest,
                        ),
                        builder.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (world, semantic, builder) in rebuilds {
            let readiness =
                self.schedule_receipt_publication(world.clone(), semantic, builder, true);
            if matches!(
                readiness,
                OperationPublication::Capacity
                    | OperationPublication::GenerationUnavailable
                    | OperationPublication::ImplementationUnavailable
                    | OperationPublication::Unavailable
            ) {
                tracing::warn!(
                    ?readiness,
                    %world,
                    "authority publication refresh could not be admitted"
                );
            }
        }
        self.authority_tick
            .send_modify(|n| *n = n.saturating_add(1));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CoreInner> {
        self.inner.lock_recovering()
    }

    fn replica_lock(&self) -> std::sync::MutexGuard<'_, replica::Replica> {
        self.replica.lock_recovering()
    }

    fn try_mutation_lane(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ()>, replica::transaction::commit::Failure> {
        match self.mutation_lane.try_lock() {
            Ok(permit) => Ok(permit),
            Err(std::sync::TryLockError::WouldBlock) => {
                Err(replica::transaction::commit::Failure::MutationBusy)
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
        }
    }

    pub(crate) fn frontier(&self) -> ReplicaFrontier {
        // The Runtime frontier names the installed immutable read image, not
        // an unpublished prepared candidate. Reading it must therefore never
        // queue behind the Replica mutation lane.
        self.lock().snapshot.frontier()
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
        let replica = self.replica_lock();
        (replica.body_count(), replica.verified_at_ms())
    }

    /// Read Replica-owned administrative state without publishing a Runtime
    /// generation. This is intentionally not used by World query, Find, live
    /// anchor resolution, or projection code: those paths pin an immutable
    /// `ReadSnapshot`/`WorldPublication` and never queue behind the mutation
    /// lane.
    pub fn with_replica_read<T>(
        &self,
        f: impl FnOnce(&replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        let replica = self.replica_lock();
        if self.lock().closed {
            return Err(replica::transaction::commit::Failure::Illegitimate(
                "station dormant".into(),
            ));
        }
        f(&replica)
    }

    /// Mutate Replica-local control state which changes neither the durable
    /// Manifest nor any readable Body (for example a pending content hold).
    pub fn with_replica_control<T>(
        &self,
        f: impl FnOnce(&mut replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        let mut replica = self.replica_lock();
        if self.lock().closed {
            return Err(replica::transaction::commit::Failure::Illegitimate(
                "station dormant".into(),
            ));
        }
        f(&mut replica)
    }

    /// Commit content-plane metadata. A changed signed root receives a new
    /// exact Runtime coordinate, while the complete Body image and every
    /// corpus branch are structurally shared.
    pub fn with_replica_metadata<T>(
        &self,
        f: impl FnOnce(&mut replica::Replica) -> Result<T, replica::transaction::commit::Failure>,
    ) -> Result<T, replica::transaction::commit::Failure> {
        let mut replica = self.replica_lock();
        let (before_root, before_frontier) = {
            let inner = self.lock();
            if inner.closed {
                return Err(replica::transaction::commit::Failure::Illegitimate(
                    "station dormant".into(),
                ));
            }
            (inner.snapshot.root(), inner.snapshot.frontier())
        };
        let result = f(&mut replica);
        if result.is_ok()
            && (replica.frontier() != before_frontier
                || Self::current_replica_root(&replica) != before_root)
        {
            let mut inner = self.lock();
            if inner.closed {
                return Err(replica::transaction::commit::Failure::Illegitimate(
                    "station dormant".into(),
                ));
            }
            let snapshot = Arc::new(replica.advance_read_snapshot_metadata(&inner.snapshot));
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
        let mut replica = self.replica_lock();
        let before = {
            let inner = self.lock();
            if inner.closed {
                return Err(replica::transaction::commit::Failure::Illegitimate(
                    "station dormant".into(),
                ));
            }
            inner.snapshot.root()
        };
        let result = f(&mut replica);
        if result.is_ok() && !changed.is_empty() {
            let mut inner = self.lock();
            if inner.closed {
                return Err(replica::transaction::commit::Failure::Illegitimate(
                    "station dormant".into(),
                ));
            }
            let snapshot = Arc::new(replica.advance_read_snapshot(&inner.snapshot, changed));
            let parent = (snapshot.root() != before)
                .then_some(before)
                .or_else(|| inner.parents.get(&before).copied().flatten());
            inner.publish_snapshot(snapshot, parent, Some(changed));
        }
        result
    }

    /*
     * LOCK ORDER: mutation permit -> `replica` -> `inner`. No code may acquire
     * an earlier lock while holding a later one. Prepared extraction retains
     * only the try-admitted permit; immutable readers pin CoreInner state and
     * then run without any of the three.
     */

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
        struct RemoteBuild {
            world_id: WorldId,
            builder: WorldPublicationBuilder,
            prior: Option<Arc<WorldPublication>>,
            snapshot: Arc<replica::ReadSnapshot>,
            id: crate::publication::WorldPublicationId,
            key: (WorldId, crate::publication::PublicationId),
            flight: Arc<PublicationFlight>,
            memory: BuildMemoryReservation,
            changed: Vec<BodyKey>,
        }

        let _mutation = self.try_mutation_lane()?;
        let mut replica = self.replica_lock();
        let mut plans = Vec::new();
        let result = {
            let inner = self.lock();
            if inner.closed {
                return Err(replica::transaction::commit::Failure::Illegitimate(
                    "station dormant".into(),
                ));
            }
            let before = inner.snapshot.root();
            drop(inner);
            let result = f(&mut replica);
            let mut inner = self.lock();
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
                    let snapshot =
                        Arc::new(replica.advance_read_snapshot(&inner.snapshot, &outcome.bodies));
                    let parent = (snapshot.root() != before)
                        .then_some(before)
                        .or_else(|| inner.parents.get(&before).copied().flatten());
                    inner.publish_snapshot(snapshot.clone(), parent, Some(&outcome.bodies));

                    // Publish the exact new read head as Building before the
                    // mutation lane is released. Old corpora are no longer
                    // current; readers either follow this flight or fail
                    // typed. Extraction itself runs below without either lock.
                    for (world_id, builder, prior) in affected {
                        let id = crate::publication::WorldPublicationId::new(
                            crate::publication::PublicationId::new(
                                snapshot.root(),
                                builder.implementation,
                                builder.extractor_schema_digest,
                            ),
                            inner.snapshot_materialization,
                        );
                        let key = (world_id.clone(), id.publication);
                        if inner.publication_flights.contains_key(&key) {
                            continue;
                        }
                        let corpus_memory = prior
                            .as_ref()
                            .filter(|prior| {
                                prior.id.publication.implementation_digest == builder.implementation
                                    && prior.id.publication.extractor_schema_digest
                                        == builder.extractor_schema_digest
                            })
                            .map(|prior| {
                                prior.corpus.estimate_delta_build_bytes(
                                    &snapshot,
                                    &world_id,
                                    &builder.extractors,
                                    &outcome.bodies,
                                )
                            })
                            .unwrap_or_else(|| {
                                crate::corpus::Corpus::estimate_build_bytes(
                                    &snapshot,
                                    &world_id,
                                    &builder.extractors,
                                )
                            });
                        let Ok(memory) = inner.reserve_build_memory(0, corpus_memory) else {
                            record_world_read_failure(
                                &mut inner,
                                (world_id, id),
                                PublicationFailure::Capacity,
                            );
                            continue;
                        };
                        let flight = Arc::new(PublicationFlight::new());
                        inner
                            .publication_flights
                            .insert(key.clone(), flight.clone());
                        inner
                            .world_read_heads
                            .insert((world_id.clone(), id), WorldReadHead::Building);
                        plans.push(RemoteBuild {
                            world_id,
                            builder,
                            prior,
                            snapshot: snapshot.clone(),
                            id,
                            key,
                            flight,
                            memory,
                            changed: outcome.bodies.clone(),
                        });
                    }
                }
            }
            result
        };
        drop(replica);

        // Remote truth is durable already. Local extraction failure cannot
        // roll it back and cannot restore the prior corpus under new coords.
        for plan in plans {
            let built = candidate_world_publication(
                plan.snapshot,
                self.body_images.clone(),
                plan.id,
                plan.prior,
                &plan.builder.world,
                &plan.world_id,
                plan.builder.implementation,
                plan.builder.extractor_schema_digest,
                &plan.builder.schemas,
                &plan.builder.extractors,
                &plan.changed,
            );
            let result = {
                let mut inner = self.lock();
                if inner
                    .publication_flights
                    .get(&plan.key)
                    .is_some_and(|current| Arc::ptr_eq(current, &plan.flight))
                {
                    inner.publication_flights.remove(&plan.key);
                }
                match built {
                    Err(failure) => {
                        record_world_read_failure(
                            &mut inner,
                            (plan.world_id.clone(), plan.id),
                            failure.clone(),
                        );
                        Err(failure)
                    }
                    Ok(publication) => {
                        if inner
                            .finish_publication_build(plan.memory, &publication)
                            .is_err()
                        {
                            record_world_read_failure(
                                &mut inner,
                                (plan.world_id.clone(), plan.id),
                                PublicationFailure::Capacity,
                            );
                            Err(PublicationFailure::Capacity)
                        } else {
                            if inner.snapshot.root() == publication.id.publication.manifest_root
                                && inner.snapshot_materialization == publication.id.materialization
                            {
                                inner.install_world_publication(
                                    plan.world_id.clone(),
                                    publication.clone(),
                                );
                            } else {
                                inner.retain_world_publication(
                                    plan.world_id.clone(),
                                    publication.clone(),
                                );
                            }
                            Ok(publication)
                        }
                    }
                }
            };
            plan.flight.complete(
                result
                    .as_ref()
                    .map(Arc::clone)
                    .map_err(|failure| find_publication_failure(failure.clone())),
            );
            if let Err(failure) = result {
                tracing::error!(
                    ?failure,
                    world = %plan.world_id,
                    "remote World publication is unavailable"
                );
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
        enum Plan {
            Ready,
            Follow(Arc<PublicationFlight>),
            Build {
                flight: Arc<PublicationFlight>,
                key: (WorldId, crate::publication::PublicationId),
                id: crate::publication::WorldPublicationId,
                snapshot: Arc<replica::ReadSnapshot>,
                build_memory: BuildMemoryReservation,
            },
        }

        // A concurrent Contact may advance the current generation while an
        // extractor is working. Finish the exact old publication for any
        // followers, retain it, then select/build the new current coordinate.
        // Registration normally runs before Sessions are exposed, so this
        // loop is almost always one pass; importantly, no extraction or cache
        // I/O happens under the Station writer.
        loop {
            let plan = {
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
                let semantic = crate::publication::PublicationId::new(
                    inner.snapshot.root(),
                    implementation,
                    extractor_schema_digest,
                );
                let id = crate::publication::WorldPublicationId::new(
                    semantic,
                    inner.snapshot_materialization,
                );
                let head = (world_id.clone(), id);
                match inner.world_read_heads.get(&head).cloned() {
                    Some(WorldReadHead::Unavailable(failure)) => return Err(failure),
                    Some(WorldReadHead::Ready)
                        if inner
                            .world_publications
                            .get(world_id)
                            .is_some_and(|publication| publication.id == id) =>
                    {
                        Plan::Ready
                    }
                    Some(WorldReadHead::Building) => {
                        let key = (world_id.clone(), semantic);
                        let Some(flight) = inner.publication_flights.get(&key).cloned() else {
                            // A prepared local/remote publication is already
                            // being installed under the writer. Do not start a
                            // parallel vocabulary or duplicate extraction.
                            return Err(PublicationFailure::Interrupted);
                        };
                        Plan::Follow(flight)
                    }
                    Some(WorldReadHead::Ready) | None => {
                        if inner
                            .world_publications
                            .get(world_id)
                            .is_some_and(|publication| publication.id == id)
                        {
                            Plan::Ready
                        } else {
                            let key = (world_id.clone(), semantic);
                            if let Some(flight) = inner.publication_flights.get(&key).cloned() {
                                Plan::Follow(flight)
                            } else {
                                let snapshot = inner.snapshot.clone();
                                let build_memory = inner
                                    .reserve_full_publication_build(
                                        &snapshot, world_id, extractors, true,
                                    )
                                    .map_err(|_| PublicationFailure::Capacity)?;
                                let flight = Arc::new(PublicationFlight::new());
                                inner
                                    .publication_flights
                                    .insert(key.clone(), flight.clone());
                                inner.world_read_heads.insert(head, WorldReadHead::Building);
                                Plan::Build {
                                    flight,
                                    key,
                                    id,
                                    snapshot,
                                    build_memory,
                                }
                            }
                        }
                    }
                }
            };

            match plan {
                Plan::Ready => return Ok(()),
                Plan::Follow(flight) => {
                    let publication = flight.wait().map_err(publication_failure_from_find)?;
                    let inner = self.lock();
                    if inner.closed {
                        return Err(PublicationFailure::Interrupted);
                    }
                    if inner.snapshot.root() == publication.id.publication.manifest_root
                        && inner.snapshot_materialization == publication.id.materialization
                    {
                        return Ok(());
                    }
                }
                Plan::Build {
                    flight,
                    key,
                    id,
                    snapshot,
                    build_memory,
                } => {
                    let built = build_world_corpus(
                        &snapshot,
                        self.body_images.clone(),
                        world,
                        world_id,
                        id,
                        schemas,
                        extractors,
                    )
                    .map(|corpus| {
                        Arc::new(WorldPublication {
                            id,
                            snapshot,
                            corpus: Arc::new(corpus),
                        })
                    });
                    let result = {
                        let mut inner = self.lock();
                        if inner
                            .publication_flights
                            .get(&key)
                            .is_some_and(|current| Arc::ptr_eq(current, &flight))
                        {
                            inner.publication_flights.remove(&key);
                        }
                        match built {
                            Err(failure) => {
                                record_world_read_failure(
                                    &mut inner,
                                    (world_id.clone(), id),
                                    failure.clone(),
                                );
                                Err(failure)
                            }
                            Ok(_publication) if inner.closed => {
                                record_world_read_failure(
                                    &mut inner,
                                    (world_id.clone(), id),
                                    PublicationFailure::Interrupted,
                                );
                                Err(PublicationFailure::Interrupted)
                            }
                            Ok(publication) => {
                                if inner
                                    .finish_publication_build(build_memory, &publication)
                                    .is_err()
                                {
                                    record_world_read_failure(
                                        &mut inner,
                                        (world_id.clone(), id),
                                        PublicationFailure::Capacity,
                                    );
                                    Err(PublicationFailure::Capacity)
                                } else {
                                    inner
                                        .world_read_heads
                                        .insert((world_id.clone(), id), WorldReadHead::Ready);
                                    if inner.snapshot.root()
                                        == publication.id.publication.manifest_root
                                        && inner.snapshot_materialization
                                            == publication.id.materialization
                                    {
                                        inner.install_world_publication(
                                            world_id.clone(),
                                            publication.clone(),
                                        );
                                    } else {
                                        inner.retain_world_publication(
                                            world_id.clone(),
                                            publication.clone(),
                                        );
                                    }
                                    Ok(publication)
                                }
                            }
                        }
                    };
                    flight.complete(
                        result
                            .as_ref()
                            .map(Arc::clone)
                            .map_err(|failure| find_publication_failure(failure.clone())),
                    );
                    let publication = result?;
                    if publication.id.publication.manifest_root == self.lock().snapshot.root() {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Start or join construction of one receipt-named semantic publication.
    /// Admission and the `Building` head are installed synchronously; all
    /// extraction runs on the composition-owned bounded publication lane with
    /// neither mutable Station lock held. Polling operation status observes
    /// the same singleflight used by Query/Find and receives a WPI only after
    /// the exact retained Arc has been installed.
    fn schedule_receipt_publication(
        self: &Arc<Self>,
        world_id: WorldId,
        semantic: crate::publication::PublicationId,
        builder: WorldPublicationBuilder,
        current_only: bool,
    ) -> OperationPublication {
        enum Source {
            Resident(Arc<replica::ReadSnapshot>),
            Cold {
                reader: replica::GenerationReader,
                footprint: replica::GenerationFootprint,
                materialization: crate::publication::MaterializationId,
            },
        }

        // Discover whether this is genuinely cold without taking the Replica
        // writer under the publication mutex. A cold reader is then pinned in
        // Replica -> Core lock order and all authenticated index I/O happens
        // after both guards have been released.
        let cold_current = {
            let inner = self.lock();
            if inner.closed {
                return OperationPublication::Unavailable;
            }
            let ready = if current_only {
                inner
                    .world_publications
                    .get(&world_id)
                    .filter(|publication| {
                        publication.id.publication == semantic
                            && publication.id.materialization == inner.snapshot_materialization
                    })
                    .cloned()
            } else {
                inner.ready_semantic_publication(&world_id, semantic)
            };
            if let Some(publication) = ready {
                return OperationPublication::Ready(publication.id);
            }
            let key = (world_id.clone(), semantic);
            if inner.publication_flights.contains_key(&key) {
                return OperationPublication::Building;
            }
            (semantic.manifest_root != inner.snapshot.root()
                && !inner.generations.contains_key(&semantic.manifest_root))
            .then(|| inner.snapshot.clone())
        };
        let cold = if let Some(current) = cold_current {
            let reader = {
                let replica = self.replica_lock();
                replica.generation_reader(current)
            };
            let footprint = match reader.generation_footprint(&semantic.manifest_root) {
                Ok(Some(footprint)) => footprint,
                Ok(None) | Err(_) => return OperationPublication::GenerationUnavailable,
            };
            Some((reader, footprint))
        } else {
            None
        };

        let (id, key, flight, source, build_memory, read_memory) = {
            let mut inner = self.lock();
            if inner.closed {
                return OperationPublication::Unavailable;
            }
            let ready = if current_only {
                inner
                    .world_publications
                    .get(&world_id)
                    .filter(|publication| {
                        publication.id.publication == semantic
                            && publication.id.materialization == inner.snapshot_materialization
                    })
                    .cloned()
            } else {
                inner.ready_semantic_publication(&world_id, semantic)
            };
            if let Some(publication) = ready {
                return OperationPublication::Ready(publication.id);
            }
            let key = (world_id.clone(), semantic);
            if inner.publication_flights.contains_key(&key) {
                return OperationPublication::Building;
            }
            let (source, materialization) = if semantic.manifest_root == inner.snapshot.root() {
                (
                    Source::Resident(inner.snapshot.clone()),
                    inner.snapshot_materialization,
                )
            } else if let Some(generation) = inner.generations.get(&semantic.manifest_root) {
                (
                    Source::Resident(generation.snapshot.clone()),
                    generation.materialization,
                )
            } else {
                let Some((reader, footprint)) = cold else {
                    return OperationPublication::GenerationUnavailable;
                };
                let materialization = inner.reserve_materialization();
                (
                    Source::Cold {
                        reader,
                        footprint,
                        materialization,
                    },
                    materialization,
                )
            };
            let id = crate::publication::WorldPublicationId::new(semantic, materialization);
            match inner.world_read_heads.get(&(world_id.clone(), id)).cloned() {
                Some(WorldReadHead::Building) => return OperationPublication::Building,
                Some(WorldReadHead::Unavailable(PublicationFailure::Capacity)) => {
                    return OperationPublication::Capacity;
                }
                Some(WorldReadHead::Unavailable(_)) => {
                    return OperationPublication::Unavailable;
                }
                Some(WorldReadHead::Ready) | None => {}
            }
            let build_memory = match &source {
                Source::Resident(snapshot) => inner.reserve_full_publication_build(
                    snapshot,
                    &world_id,
                    &builder.extractors,
                    true,
                ),
                Source::Cold { footprint, .. } => inner.reserve_historical_publication_build(
                    footprint,
                    &world_id,
                    &builder.extractors,
                ),
            };
            let build_memory = match build_memory {
                Ok(memory) => memory,
                Err(()) => return OperationPublication::Capacity,
            };
            let flight = Arc::new(PublicationFlight::new());
            inner
                .publication_flights
                .insert(key.clone(), flight.clone());
            inner
                .world_read_heads
                .insert((world_id.clone(), id), WorldReadHead::Building);
            (
                id,
                key,
                flight,
                source,
                build_memory,
                inner.read_memory.clone(),
            )
        };

        let core = self.clone();
        let worker_flight = flight.clone();
        let worker_key = key.clone();
        let worker_world = world_id.clone();
        let job: PublicationJob = Box::new(move || {
            let resolved = match source {
                Source::Resident(snapshot) => Ok((snapshot, id)),
                Source::Cold {
                    reader,
                    materialization,
                    ..
                } => match reader.read_generation(&semantic.manifest_root) {
                    Ok(Some(snapshot)) => {
                        let reconstructed = Arc::new(snapshot);
                        let mut inner = core.lock();
                        if inner.closed {
                            Err(PublicationFailure::Interrupted)
                        } else if let Some(cached) =
                            inner.generations.get(&semantic.manifest_root).cloned()
                        {
                            let actual = crate::publication::WorldPublicationId::new(
                                semantic,
                                cached.materialization,
                            );
                            if actual != id {
                                inner.world_read_heads.remove(&(worker_world.clone(), id));
                                inner.world_read_heads.insert(
                                    (worker_world.clone(), actual),
                                    WorldReadHead::Building,
                                );
                            }
                            Ok((cached.snapshot, actual))
                        } else {
                            inner.cache_generation_at(
                                semantic.manifest_root,
                                reconstructed.clone(),
                                None,
                                materialization,
                            );
                            Ok((reconstructed, id))
                        }
                    }
                    Ok(None) | Err(_) => Err(PublicationFailure::Generation),
                },
            };
            let (built, completed_id) = match resolved {
                Ok((snapshot, actual_id)) => {
                    let built = build_world_corpus(
                        &snapshot,
                        core.body_images.clone(),
                        &builder.world,
                        &worker_world,
                        actual_id,
                        &builder.schemas,
                        &builder.extractors,
                    )
                    .map(|corpus| {
                        Arc::new(WorldPublication {
                            id: actual_id,
                            snapshot,
                            corpus: Arc::new(corpus),
                        })
                    });
                    (built, actual_id)
                }
                Err(failure) => (Err(failure), id),
            };
            let result = {
                let mut inner = core.lock();
                if inner
                    .publication_flights
                    .get(&worker_key)
                    .is_some_and(|current| Arc::ptr_eq(current, &worker_flight))
                {
                    inner.publication_flights.remove(&worker_key);
                }
                match built {
                    Err(failure) => {
                        record_world_read_failure(
                            &mut inner,
                            (worker_world.clone(), completed_id),
                            failure.clone(),
                        );
                        Err(failure)
                    }
                    Ok(_publication) if inner.closed => {
                        record_world_read_failure(
                            &mut inner,
                            (worker_world.clone(), completed_id),
                            PublicationFailure::Interrupted,
                        );
                        Err(PublicationFailure::Interrupted)
                    }
                    Ok(publication) => {
                        if inner
                            .finish_publication_build(build_memory, &publication)
                            .is_err()
                        {
                            record_world_read_failure(
                                &mut inner,
                                (worker_world.clone(), completed_id),
                                PublicationFailure::Capacity,
                            );
                            Err(PublicationFailure::Capacity)
                        } else {
                            inner
                                .world_read_heads
                                .insert((worker_world.clone(), completed_id), WorldReadHead::Ready);
                            if inner.snapshot.root() == completed_id.publication.manifest_root
                                && inner.snapshot_materialization == completed_id.materialization
                            {
                                inner.install_world_publication(
                                    worker_world.clone(),
                                    publication.clone(),
                                );
                            } else {
                                inner.retain_world_publication(
                                    worker_world.clone(),
                                    publication.clone(),
                                );
                            }
                            Ok(publication)
                        }
                    }
                }
            };
            worker_flight.complete(
                result
                    .as_ref()
                    .map(Arc::clone)
                    .map_err(|failure| find_publication_failure(failure.clone())),
            );
        });
        if let Err(job) = read_memory.schedule_publication(job) {
            drop(job);
            let mut inner = self.lock();
            if inner
                .publication_flights
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &flight))
            {
                inner.publication_flights.remove(&key);
            }
            inner.world_read_heads.remove(&(world_id, id));
            drop(inner);
            flight.complete(Err(crate::find::Failure::CursorCapacityExceeded));
            return OperationPublication::Capacity;
        }
        OperationPublication::Building
    }

    /// Close the core to further commits, as one transition under the writer
    /// mutex: an in-flight submit either completed its journaled durable commit
    /// before the close or observes it and is refused. Every acknowledged
    /// commit is already on disk, so closing needs no checkpoint. Observation
    /// streams end with a typed `StationDormant`.
    pub(crate) fn close(&self) {
        let _replica = self.replica_lock();
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
    fn anchor_in_body(
        &self,
        key: &BodyKey,
        path: &str,
        position: u64,
    ) -> Result<Option<fabric::Anchor>, crate::world::BodyReadFailure> {
        let inner = self.lock();
        if inner.closed {
            return Err(crate::world::BodyReadFailure::Interrupted(
                crate::world::BodyReadCoordinate::new(key.clone(), None),
            ));
        }
        let snapshot = inner.snapshot.clone();
        drop(inner);
        let reader = SnapshotReader::interactive(snapshot, self.body_images.clone());
        crate::world::BodyReader::anchor_in_body(&reader, key, path, position)
    }

    fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, crate::world::BodyReadFailure> {
        let inner = self.lock();
        if inner.closed {
            return Err(crate::world::BodyReadFailure::Interrupted(
                crate::world::BodyReadCoordinate::new(key.clone(), None),
            ));
        }
        let snapshot = inner.snapshot.clone();
        drop(inner);
        let reader = SnapshotReader::interactive(snapshot, self.body_images.clone());
        crate::world::BodyReader::resolve_anchor(&reader, key, anchor)
    }
}

/// Test-only resident reader for small in-memory Replica fixtures. Production
/// callbacks always use `SnapshotReader`; there is intentionally no raw
/// Replica fallback around cold-image admission.
#[cfg(test)]
struct ReplicaReader<'a> {
    replica: &'a replica::Replica,
    snapshot: &'a replica::ReadSnapshot,
}

#[cfg(test)]
impl crate::exec::ReservedBodyReader for ReplicaReader<'_> {
    fn content_descriptor(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<replica::content::ContentDescriptor> {
        self.snapshot.content_descriptor(content)
    }

    fn binding(&self, key: &BodyKey) -> Option<replica::body::BodyBinding> {
        self.replica.binding(key).cloned()
    }

    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        self.snapshot
            .body_keys_page_with_schema(world, schema, after, limit)
    }

    fn read_atomic(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure> {
        let Some(binding) = self.replica.binding(key) else {
            return Ok(None);
        };
        let coordinate = crate::world::BodyReadCoordinate::new(key.clone(), None);
        if self.replica.is_opaque(key) {
            return Err(crate::world::BodyReadFailure::Opaque(coordinate));
        }
        if binding.mutation_model != replica::body::MUTATION_ATOMIC
            && binding.mutation_model != replica::body::MUTATION_IMMUTABLE_ATOMIC
        {
            return Err(crate::world::BodyReadFailure::Corrupt(coordinate));
        }
        self.replica
            .read(key)
            .map(crate::world::BodyBytes::owned)
            .map(Some)
            .ok_or(crate::world::BodyReadFailure::Corrupt(coordinate))
    }

    fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure> {
        let Some(binding) = self.replica.binding(key) else {
            return Ok(None);
        };
        let coordinate = crate::world::BodyReadCoordinate::new(key.clone(), None);
        if self.replica.is_opaque(key) {
            return Err(crate::world::BodyReadFailure::Opaque(coordinate));
        }
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(coordinate));
        }
        self.replica
            .read_collaborative(key)
            .map(crate::world::CollaborativeBody::owned)
            .map(Some)
            .map_err(|failure| projection_body_failure(key, None, failure))
    }
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

fn mutation_model_tag(model: &MutationModel) -> u8 {
    match model {
        MutationModel::Atomic => replica::body::MUTATION_ATOMIC,
        MutationModel::ImmutableAtomic => replica::body::MUTATION_IMMUTABLE_ATOMIC,
        MutationModel::Collaborative(_) => replica::body::MUTATION_COLLABORATIVE,
    }
}

fn operation_matches_mutation_model(model: &MutationModel, operation: &Op) -> bool {
    match (model, operation) {
        (MutationModel::Atomic, Op::ReplaceAtomic { .. } | Op::Tombstone) => true,
        (MutationModel::ImmutableAtomic, Op::ReplaceAtomic { .. }) => true,
        (MutationModel::Collaborative(_), Op::Create | Op::Tombstone) => true,
        (MutationModel::Collaborative(_), Op::ReplaceAtomic { .. }) => false,
        (MutationModel::Collaborative(_), _) => true,
        (MutationModel::Atomic | MutationModel::ImmutableAtomic, _) => false,
    }
}

#[derive(Debug, Default)]
struct RuntimeEffect {
    operations: Vec<(BodyKey, Op)>,
    bindings: Vec<(BodyKey, replica::body::BodyBinding)>,
    content_refs: Vec<(BodyKey, Vec<replica::content::ContentRef>)>,
    bodies: Vec<BodyKey>,
    demands: Vec<Vec<u8>>,
}

/// The host's bounded Find delegate for one dispatched Attempt.
///
/// It pins every query to the publication the Run started against and then
/// delegates to the same admission and evaluator as ordinary
/// [`Session::find`] — the Session's own policy and gates still apply and
/// can only narrow further. Grant and budget admission happened in
/// `exec::Context::query` before this is reached.
struct AttemptFindDelegate<'a> {
    session: &'a Session,
    pinned: crate::publication::PublicationId,
}

impl crate::exec::FindDelegate for AttemptFindDelegate<'_> {
    fn find(
        &self,
        mut query: crate::find::Query,
    ) -> Result<crate::find::Answer, crate::find::Failure> {
        if query
            .publication
            .is_some_and(|requested| requested != self.pinned)
        {
            return Err(crate::find::Invalid::InvalidQuery("publication").into());
        }
        query.publication = Some(self.pinned);
        self.session.find(query)
    }
}

fn read_failure(failure: crate::exec::ReadFailure) -> Failure {
    match failure {
        crate::exec::ReadFailure::Invalid(invalid) => Failure::Rejected(exec_invalid(invalid)),
        crate::exec::ReadFailure::Body(body) => Failure::PersistenceCause {
            operation: "read reserved Exec state",
            reason: format!("{body:?}"),
        },
    }
}

fn exec_invalid(invalid: crate::exec::Invalid) -> Rejection {
    match invalid {
        crate::exec::Invalid::TooLarge => Rejection::LimitExceeded,
        _ => Rejection::ContractViolation,
    }
}

fn exec_read_rejection(failure: crate::exec::ReadFailure) -> Rejection {
    match failure {
        crate::exec::ReadFailure::Invalid(invalid) => exec_invalid(invalid),
        crate::exec::ReadFailure::Body(failure) => Rejection::BodyRead(failure),
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
    snapshot: &impl crate::exec::ReservedBodyReader,
    world: &WorldId,
    run: crate::exec::RunId,
) -> Result<LowerRun, Rejection> {
    let (run, start, event_count) = crate::exec::read_committed_run(snapshot, world, run)
        .map_err(exec_read_rejection)?
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

fn active_run_binding() -> Result<replica::body::BodyBinding, Rejection> {
    Ok(replica::body::BodyBinding {
        schema: SchemaId::parse(crate::exec::ACTIVE_RUN_BODY_SCHEMA)
            .ok_or(Rejection::ContractViolation)?,
        schema_version: crate::exec::ACTIVE_RUN_BODY_SCHEMA_VERSION,
        encoding: EncodingId::parse(crate::exec::BODY_ENCODING)
            .ok_or(Rejection::ContractViolation)?,
        mutation_model: replica::body::MUTATION_ATOMIC,
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
    snapshot: &impl crate::exec::ReservedBodyReader,
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
    snapshot: &impl crate::exec::ReservedBodyReader,
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
                let active = crate::exec::active_run_body_key(&ambient.world, run);
                lowered.operations.push((
                    active.clone(),
                    Op::ReplaceAtomic {
                        value: run.as_bytes().to_vec(),
                    },
                ));
                lowered
                    .bindings
                    .push((active.clone(), active_run_binding()?));
                lowered.bodies.push(active);
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
                let active = crate::exec::active_run_body_key(&ambient.world, *run);
                lowered.operations.push((active.clone(), Op::Tombstone));
                lowered
                    .bindings
                    .push((active.clone(), active_run_binding()?));
                lowered.bodies.push(active);
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
    snapshot: &impl crate::exec::ReservedBodyReader,
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
    snapshot: &impl crate::exec::ReservedBodyReader,
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
    snapshot: &impl crate::exec::ReservedBodyReader,
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
        | replica::transaction::commit::Failure::ImmutableConflict
        | replica::transaction::commit::Failure::ParentManifestUnavailable => {
            Failure::Conflict(Conflict::Body)
        }
        replica::transaction::commit::Failure::SchemaMismatch => {
            Failure::Rejected(Rejection::ContractViolation)
        }
        replica::transaction::commit::Failure::RequestIdConflict => {
            Failure::Conflict(Conflict::Request)
        }
        replica::transaction::commit::Failure::ReceiptCheckStale => {
            Failure::Conflict(Conflict::Body)
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
        replica::transaction::commit::Failure::NeedsSemanticMigration { bodies } => {
            Failure::PersistenceCause {
                operation: "open exact Body generation",
                reason: format!(
                    "{bodies} legacy Body image(s) require the composition-owned migration step"
                ),
            }
        }
        replica::transaction::commit::Failure::OutcomeUnknown => Failure::OutcomeUnknown,
        replica::transaction::commit::Failure::MutationBusy => Failure::Busy,
        replica::transaction::commit::Failure::Illegitimate(_)
        | replica::transaction::commit::Failure::IllegitimateContact { .. }
        | replica::transaction::commit::Failure::Engine(_)
        | replica::transaction::commit::Failure::Integrity(_)
        | replica::transaction::commit::Failure::Body(_)
        | replica::transaction::commit::Failure::CheckpointBackpressure
        | replica::transaction::commit::Failure::BodyKeyUnavailable
        | replica::transaction::commit::Failure::Durability(_)
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
        crate::find::Failure::CursorCapacityExceeded => Failure::ReadCapacity,
        other => Failure::PersistenceCause {
            operation: "resolve exact World query publication",
            reason: format!("{other:?}"),
        },
    }
}

fn find_publication_failure(error: PublicationFailure) -> crate::find::Failure {
    match error {
        PublicationFailure::Interrupted => crate::find::Failure::Interrupted,
        PublicationFailure::Capacity => crate::find::Failure::CursorCapacityExceeded,
        PublicationFailure::Generation => crate::find::Failure::PublicationUnavailable,
        PublicationFailure::Extractor {
            rejection: Some(Rejection::BodyRead(crate::world::BodyReadFailure::Capacity(_))),
            ..
        } => crate::find::Failure::CursorCapacityExceeded,
        PublicationFailure::Extractor {
            rejection: Some(Rejection::BodyRead(crate::world::BodyReadFailure::Interrupted(_))),
            ..
        } => crate::find::Failure::Interrupted,
        PublicationFailure::Extractor {
            rejection:
                Some(Rejection::BodyRead(crate::world::BodyReadFailure::PublicationExpired(_))),
            ..
        } => crate::find::Failure::PublicationExpired,
        PublicationFailure::Extractor { .. } | PublicationFailure::Corpus => {
            crate::find::Failure::Unavailable
        }
    }
}

fn session_publication_failure(error: PublicationFailure, operation: &'static str) -> Failure {
    match error {
        PublicationFailure::Interrupted => Failure::Interrupted,
        PublicationFailure::Capacity => Failure::ReadCapacity,
        PublicationFailure::Generation => Failure::GenerationUnavailable,
        PublicationFailure::Extractor {
            rejection: Some(Rejection::BodyRead(failure)),
            ..
        } => Failure::Rejected(Rejection::BodyRead(failure)),
        cause @ (PublicationFailure::Extractor { .. } | PublicationFailure::Corpus) => {
            Failure::PersistenceCause {
                operation,
                reason: format!("{cause:?}"),
            }
        }
    }
}

/// A current ambient write meeting a `Building` publication is live but not
/// yet admissible: Contact or another publication builder has advanced durable
/// truth and the write must not overtake it. Surface bounded contention, not a
/// false Station shutdown, so clients preserve their view and retry explicitly.
fn submission_publication_failure(error: PublicationFailure, operation: &'static str) -> Failure {
    match error {
        PublicationFailure::Interrupted => Failure::Busy,
        other => session_publication_failure(other, operation),
    }
}

fn publication_failure_from_find(error: crate::find::Failure) -> PublicationFailure {
    match error {
        crate::find::Failure::Interrupted => PublicationFailure::Interrupted,
        crate::find::Failure::CursorCapacityExceeded => PublicationFailure::Capacity,
        crate::find::Failure::PublicationUnavailable | crate::find::Failure::PublicationExpired => {
            PublicationFailure::Generation
        }
        crate::find::Failure::Unavailable => {
            extractor_publication_failure(None, None, "find-projection")
        }
        _ => PublicationFailure::Corpus,
    }
}

fn projection_body_failure(
    key: &BodyKey,
    material: Option<replica::BodyImageId>,
    failure: fabric::projection::Failure,
) -> crate::world::BodyReadFailure {
    let coordinate = crate::world::BodyReadCoordinate::new(
        key.clone(),
        material.map(replica::BodyImageId::as_bytes),
    );
    match failure {
        fabric::projection::Failure::NotCollaborative => {
            crate::world::BodyReadFailure::NotCollaborative(coordinate)
        }
        fabric::projection::Failure::SchemaAhead => {
            crate::world::BodyReadFailure::SchemaAhead(coordinate)
        }
        fabric::projection::Failure::Malformed => {
            crate::world::BodyReadFailure::Corrupt(coordinate)
        }
    }
}

#[cfg(test)]
impl crate::world::BodyReader for ReplicaReader<'_> {
    fn read_body(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure> {
        let Some(binding) = self.replica.binding(key) else {
            return Ok(None);
        };
        let coordinate = crate::world::BodyReadCoordinate::new(key.clone(), None);
        if crate::exec::is_reserved_schema(&binding.schema) || self.replica.is_opaque(key) {
            return Err(crate::world::BodyReadFailure::Opaque(coordinate));
        }
        self.replica
            .read(key)
            .map(crate::world::BodyBytes::owned)
            .map(Some)
            .ok_or(crate::world::BodyReadFailure::Opaque(coordinate))
    }
    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure> {
        let Some(binding) = self.replica.binding(key) else {
            return Ok(None);
        };
        let coordinate = crate::world::BodyReadCoordinate::new(key.clone(), None);
        if crate::exec::is_reserved_schema(&binding.schema) || self.replica.is_opaque(key) {
            return Err(crate::world::BodyReadFailure::Opaque(coordinate));
        }
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(coordinate));
        }
        self.replica
            .read_collaborative(key)
            .map(crate::world::CollaborativeBody::owned)
            .map(Some)
            .map_err(|failure| projection_body_failure(key, None, failure))
    }
    fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        world_readable(self.replica.binding(key)).then(|| self.replica.body_version(key))?
    }
    fn anchor_in_body(
        &self,
        key: &BodyKey,
        path: &str,
        position: u64,
    ) -> Result<Option<fabric::Anchor>, crate::world::BodyReadFailure> {
        let Some(binding) = self.replica.binding(key) else {
            return Ok(None);
        };
        let coordinate = crate::world::BodyReadCoordinate::new(key.clone(), None);
        if crate::exec::is_reserved_schema(&binding.schema) || self.replica.is_opaque(key) {
            return Err(crate::world::BodyReadFailure::Opaque(coordinate));
        }
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(coordinate));
        }
        Ok(self.replica.anchor(key, path, position))
    }
    fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, crate::world::BodyReadFailure> {
        let Some(binding) = self.replica.binding(key) else {
            return Ok(fabric::AnchorResolution::Drifted);
        };
        let coordinate = crate::world::BodyReadCoordinate::new(key.clone(), None);
        if crate::exec::is_reserved_schema(&binding.schema) || self.replica.is_opaque(key) {
            return Err(crate::world::BodyReadFailure::Opaque(coordinate));
        }
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(coordinate));
        }
        Ok(self.replica.resolve_anchor(key, anchor))
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
    ) -> Result<Option<crate::world::OutcomeFacts>, crate::world::BodyReadFailure> {
        crate::exec::outcome_facts(self, world, run, attempt).map_err(|failure| match failure {
            crate::exec::ReadFailure::Body(failure) => failure,
            crate::exec::ReadFailure::Invalid(_) => {
                crate::world::BodyReadFailure::Corrupt(crate::world::BodyReadCoordinate::new(
                    BodyKey {
                        world: world.clone(),
                        body: BodyId::from_bytes(run.as_bytes()),
                    },
                    None,
                ))
            }
        })
    }
}

/// Whether an exact snapshot read should participate in the Station's
/// interactive hot set. Publication extraction has already reserved its
/// streaming transient and deliberately does not fill the cache.
enum SnapshotReadMode {
    Interactive(Arc<crate::body_image::BodyImageCache>),
    StreamingNoFill(Arc<crate::body_image::BodyImageCache>),
}

/// A [`BodyReader`] over an immutable generation. Unlike [`ReplicaReader`],
/// this owns no borrow of either Station mutex. Protected-object reads and
/// decryption therefore happen only after the exact snapshot has been pinned
/// and every Station/Replica lock has been released.
struct SnapshotReader {
    snapshot: Arc<replica::ReadSnapshot>,
    mode: SnapshotReadMode,
}

impl SnapshotReader {
    fn interactive(
        snapshot: Arc<replica::ReadSnapshot>,
        body_images: Arc<crate::body_image::BodyImageCache>,
    ) -> Self {
        Self {
            snapshot,
            mode: SnapshotReadMode::Interactive(body_images),
        }
    }

    fn streaming(
        snapshot: Arc<replica::ReadSnapshot>,
        body_images: Arc<crate::body_image::BodyImageCache>,
    ) -> Self {
        Self {
            snapshot,
            mode: SnapshotReadMode::StreamingNoFill(body_images),
        }
    }

    fn coordinate(
        &self,
        key: &BodyKey,
        material: Option<replica::BodyImageId>,
    ) -> crate::world::BodyReadCoordinate {
        crate::world::BodyReadCoordinate::new(
            key.clone(),
            material.map(replica::BodyImageId::as_bytes),
        )
    }

    fn replica_failure(
        &self,
        key: &BodyKey,
        material: replica::BodyImageId,
        failure: replica::BodyImageFailure,
    ) -> crate::world::BodyReadFailure {
        let coordinate = self.coordinate(key, Some(material));
        match failure {
            replica::BodyImageFailure::Opaque => crate::world::BodyReadFailure::Opaque(coordinate),
            replica::BodyImageFailure::KeyUnavailable => {
                crate::world::BodyReadFailure::KeyUnavailable(coordinate)
            }
            replica::BodyImageFailure::Capacity => {
                crate::world::BodyReadFailure::Capacity(coordinate)
            }
            replica::BodyImageFailure::Corrupt
            | replica::BodyImageFailure::ModelMismatch
            | replica::BodyImageFailure::ImmutableConflict => {
                crate::world::BodyReadFailure::Corrupt(coordinate)
            }
            replica::BodyImageFailure::MaterialUnavailable | replica::BodyImageFailure::Io => {
                crate::world::BodyReadFailure::MaterialUnavailable(coordinate)
            }
        }
    }

    fn cache_failure(
        &self,
        key: &BodyKey,
        material: replica::BodyImageId,
        failure: crate::body_image::BodyImageFailure,
    ) -> crate::world::BodyReadFailure {
        let coordinate = self.coordinate(key, Some(material));
        match failure {
            crate::body_image::BodyImageFailure::Capacity => {
                crate::world::BodyReadFailure::Capacity(coordinate)
            }
            crate::body_image::BodyImageFailure::KeyUnavailable => {
                crate::world::BodyReadFailure::KeyUnavailable(coordinate)
            }
            crate::body_image::BodyImageFailure::Corrupt => {
                crate::world::BodyReadFailure::Corrupt(coordinate)
            }
            crate::body_image::BodyImageFailure::NotCollaborative => {
                crate::world::BodyReadFailure::NotCollaborative(coordinate)
            }
            crate::body_image::BodyImageFailure::SchemaAhead => {
                crate::world::BodyReadFailure::SchemaAhead(coordinate)
            }
            crate::body_image::BodyImageFailure::Opaque => {
                crate::world::BodyReadFailure::Opaque(coordinate)
            }
            crate::body_image::BodyImageFailure::Unavailable => {
                crate::world::BodyReadFailure::MaterialUnavailable(coordinate)
            }
            crate::body_image::BodyImageFailure::Interrupted => {
                crate::world::BodyReadFailure::Interrupted(coordinate)
            }
        }
    }

    fn resolve_image(
        &self,
        key: &BodyKey,
        body: replica::BodyIx,
        material: replica::BodyImageId,
    ) -> Result<crate::body_image::PinnedBodyImage, crate::world::BodyReadFailure> {
        let bounds = self.snapshot.body_image_bounds(body).ok_or_else(|| {
            crate::world::BodyReadFailure::MaterialUnavailable(self.coordinate(key, Some(material)))
        })?;
        match &self.mode {
            SnapshotReadMode::Interactive(cache) => {
                let snapshot = self.snapshot.clone();
                cache
                    .resolve(
                        crate::body_image::BodyImageAdmission {
                            key: material.into(),
                            protected_bytes: bounds.protected_bytes,
                            decoded_upper_bound: bounds.decoded_upper_bound,
                        },
                        move || snapshot.resolve_body_image(body).map_err(Into::into),
                    )
                    .map_err(|failure| self.cache_failure(key, material, failure))
            }
            SnapshotReadMode::StreamingNoFill(cache) => {
                let snapshot = self.snapshot.clone();
                cache
                    .resolve_no_fill(
                        crate::body_image::BodyImageAdmission {
                            key: material.into(),
                            protected_bytes: bounds.protected_bytes,
                            decoded_upper_bound: bounds.decoded_upper_bound,
                        },
                        move || snapshot.resolve_body_image(body).map_err(Into::into),
                    )
                    .map_err(|failure| self.cache_failure(key, material, failure))
            }
        }
    }

    fn read_reserved_atomic(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure> {
        let (body, material) = match self.snapshot.body_presence(key) {
            replica::BodyImagePresence::Absent => return Ok(None),
            replica::BodyImagePresence::Opaque { image } => {
                return Err(crate::world::BodyReadFailure::Opaque(
                    self.coordinate(key, Some(image)),
                ));
            }
            replica::BodyImagePresence::Readable { body, image } => (body, image),
        };
        let binding = self.snapshot.binding(key).ok_or_else(|| {
            crate::world::BodyReadFailure::Corrupt(self.coordinate(key, Some(material)))
        })?;
        if binding.mutation_model != replica::body::MUTATION_ATOMIC
            && binding.mutation_model != replica::body::MUTATION_IMMUTABLE_ATOMIC
        {
            return Err(crate::world::BodyReadFailure::Corrupt(
                self.coordinate(key, Some(material)),
            ));
        }
        let image = self.resolve_image(key, body, material)?;
        let bytes = image
            .read_shared()
            .map(|bytes| crate::world::BodyBytes::cached(bytes, image));
        bytes.map(Some).ok_or_else(|| {
            crate::world::BodyReadFailure::Corrupt(self.coordinate(key, Some(material)))
        })
    }

    fn read_reserved_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure> {
        let (body, material) = match self.snapshot.body_presence(key) {
            replica::BodyImagePresence::Absent => return Ok(None),
            replica::BodyImagePresence::Opaque { image } => {
                return Err(crate::world::BodyReadFailure::Opaque(
                    self.coordinate(key, Some(image)),
                ));
            }
            replica::BodyImagePresence::Readable { body, image } => (body, image),
        };
        let binding = self.snapshot.binding(key).ok_or_else(|| {
            crate::world::BodyReadFailure::Corrupt(self.coordinate(key, Some(material)))
        })?;
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(
                self.coordinate(key, Some(material)),
            ));
        }
        let image = self.resolve_image(key, body, material)?;
        image
            .read_collaborative()
            .map(|view| crate::world::CollaborativeBody::cached(view, image))
            .map(Some)
            .map_err(|failure| self.cache_failure(key, material, failure))
    }
}

impl crate::exec::ReservedBodyReader for SnapshotReader {
    fn content_descriptor(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<replica::content::ContentDescriptor> {
        self.snapshot.content_descriptor(content)
    }

    fn binding(&self, key: &BodyKey) -> Option<replica::body::BodyBinding> {
        self.snapshot.binding(key).cloned()
    }

    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        self.snapshot
            .body_keys_page_with_schema(world, schema, after, limit)
    }

    fn read_atomic(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure> {
        self.read_reserved_atomic(key)
    }

    fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure> {
        self.read_reserved_collaborative(key)
    }
}

impl crate::world::BodyReader for SnapshotReader {
    fn read_body(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure> {
        let (body, material) = match self.snapshot.body_presence(key) {
            replica::BodyImagePresence::Absent => return Ok(None),
            replica::BodyImagePresence::Opaque { image } => {
                return Err(crate::world::BodyReadFailure::Opaque(
                    self.coordinate(key, Some(image)),
                ));
            }
            replica::BodyImagePresence::Readable { body, image } => (body, image),
        };
        let Some(binding) = self.snapshot.binding(key) else {
            return Err(crate::world::BodyReadFailure::Corrupt(
                self.coordinate(key, Some(material)),
            ));
        };
        if crate::exec::is_reserved_schema(&binding.schema) {
            return Err(crate::world::BodyReadFailure::Opaque(
                self.coordinate(key, Some(material)),
            ));
        }
        let image = self.resolve_image(key, body, material)?;
        let bytes = image
            .read_shared()
            .map(|bytes| crate::world::BodyBytes::cached(bytes, image));
        bytes.map(Some).ok_or_else(|| {
            crate::world::BodyReadFailure::Opaque(self.coordinate(key, Some(material)))
        })
    }
    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure> {
        let (body, material) = match self.snapshot.body_presence(key) {
            replica::BodyImagePresence::Absent => return Ok(None),
            replica::BodyImagePresence::Opaque { image } => {
                return Err(crate::world::BodyReadFailure::Opaque(
                    self.coordinate(key, Some(image)),
                ));
            }
            replica::BodyImagePresence::Readable { body, image } => (body, image),
        };
        let Some(binding) = self.snapshot.binding(key) else {
            return Err(crate::world::BodyReadFailure::Corrupt(
                self.coordinate(key, Some(material)),
            ));
        };
        if crate::exec::is_reserved_schema(&binding.schema) {
            return Err(crate::world::BodyReadFailure::Opaque(
                self.coordinate(key, Some(material)),
            ));
        }
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(
                self.coordinate(key, Some(material)),
            ));
        }
        let image = self.resolve_image(key, body, material)?;
        image
            .read_collaborative()
            .map(|view| crate::world::CollaborativeBody::cached(view, image))
            .map(Some)
            .map_err(|failure| self.cache_failure(key, material, failure))
    }
    fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        world_readable(self.snapshot.binding(key)).then(|| self.snapshot.body_version(key))?
    }
    fn anchor_in_body(
        &self,
        key: &BodyKey,
        path: &str,
        position: u64,
    ) -> Result<Option<fabric::Anchor>, crate::world::BodyReadFailure> {
        let (body, material) = match self.snapshot.body_presence(key) {
            replica::BodyImagePresence::Absent => return Ok(None),
            replica::BodyImagePresence::Opaque { image } => {
                return Err(crate::world::BodyReadFailure::Opaque(
                    self.coordinate(key, Some(image)),
                ));
            }
            replica::BodyImagePresence::Readable { body, image } => (body, image),
        };
        let Some(binding) = self.snapshot.binding(key) else {
            return Err(crate::world::BodyReadFailure::Corrupt(
                self.coordinate(key, Some(material)),
            ));
        };
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(
                self.coordinate(key, Some(material)),
            ));
        }
        let image = self.resolve_image(key, body, material)?;
        // Keep both the canonical-image pin and its governed decoded
        // projection lease alive through the no-I/O anchor operation.
        let _projection = image
            .read_collaborative()
            .map_err(|failure| self.cache_failure(key, material, failure))?;
        replica::ReadSnapshot::anchor_in_resolved_image(key, image.snapshot(), path, position)
            .map_err(|failure| projection_body_failure(key, Some(material), failure))
    }
    fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, crate::world::BodyReadFailure> {
        let (body, material) = match self.snapshot.body_presence(key) {
            replica::BodyImagePresence::Absent => return Ok(fabric::AnchorResolution::Drifted),
            replica::BodyImagePresence::Opaque { image } => {
                return Err(crate::world::BodyReadFailure::Opaque(
                    self.coordinate(key, Some(image)),
                ));
            }
            replica::BodyImagePresence::Readable { body, image } => (body, image),
        };
        let Some(binding) = self.snapshot.binding(key) else {
            return Err(crate::world::BodyReadFailure::Corrupt(
                self.coordinate(key, Some(material)),
            ));
        };
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(
                self.coordinate(key, Some(material)),
            ));
        }
        let image = self.resolve_image(key, body, material)?;
        let _projection = image
            .read_collaborative()
            .map_err(|failure| self.cache_failure(key, material, failure))?;
        replica::ReadSnapshot::resolve_anchor_in_resolved_image(key, image.snapshot(), anchor)
            .map_err(|failure| projection_body_failure(key, Some(material), failure))
    }
    fn content_status(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<crate::world::ContentStatus> {
        self.snapshot
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
        world_readable(self.snapshot.binding(key)).then(|| self.snapshot.body_stamp(key))?
    }
    fn outcome(
        &self,
        world: &WorldId,
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
    ) -> Result<Option<crate::world::OutcomeFacts>, crate::world::BodyReadFailure> {
        crate::exec::outcome_facts(self, world, run, attempt).map_err(|failure| match failure {
            crate::exec::ReadFailure::Body(failure) => failure,
            crate::exec::ReadFailure::Invalid(_) => {
                crate::world::BodyReadFailure::Corrupt(crate::world::BodyReadCoordinate::new(
                    BodyKey {
                        world: world.clone(),
                        body: BodyId::from_bytes(run.as_bytes()),
                    },
                    None,
                ))
            }
        })
    }
}

fn corpus_image_key(
    snapshot: &replica::ReadSnapshot,
    world: &WorldId,
    publication: crate::publication::PublicationId,
    extractors: &[crate::find::Extractor],
) -> crate::corpus_store::CorpusImageKey {
    let sources: Vec<_> = extractors
        .iter()
        .map(|extractor| (extractor.source.name.clone(), extractor.source.version))
        .collect();
    crate::corpus_store::CorpusImageKey {
        publication,
        source_fingerprint: snapshot.source_fingerprint(world, &sources),
    }
}

#[allow(clippy::too_many_arguments)]
fn ready_inner_world_publication(
    inner: &CoreInner,
    world_id: &WorldId,
    implementation: [u8; 32],
    extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
) -> Result<Arc<WorldPublication>, PublicationFailure> {
    let id = crate::publication::WorldPublicationId::new(
        crate::publication::PublicationId::new(
            inner.snapshot.root(),
            implementation,
            extractor_schema_digest,
        ),
        inner.snapshot_materialization,
    );
    let head = (world_id.clone(), id);
    match inner.world_read_heads.get(&head).cloned() {
        Some(WorldReadHead::Unavailable(failure)) => return Err(failure),
        Some(WorldReadHead::Building) => return Err(PublicationFailure::Interrupted),
        Some(WorldReadHead::Ready) | None => {}
    }
    if let Some(publication) = inner
        .world_publications
        .get(world_id)
        .filter(|publication| publication.id == id)
        .cloned()
    {
        return Ok(publication);
    }
    Err(PublicationFailure::Interrupted)
}

fn build_world_corpus(
    snapshot: &Arc<replica::ReadSnapshot>,
    body_images: Arc<crate::body_image::BodyImageCache>,
    world: &Arc<dyn World>,
    world_id: &WorldId,
    id: crate::publication::WorldPublicationId,
    schemas: &[crate::find::Schema],
    extractors: &[crate::find::Extractor],
) -> Result<crate::corpus::Corpus, PublicationFailure> {
    let reader = SnapshotReader::streaming(snapshot.clone(), body_images);
    let context = crate::world::ExtractionContext::new(&reader, world_id, id);
    let mut by_source =
        std::collections::BTreeMap::<crate::find::SourceRef, Vec<&crate::find::Extractor>>::new();
    for extractor in extractors {
        if !schemas.iter().any(|schema| {
            schema.reference == extractor.schema && schema.sources.contains(&extractor.source)
        }) {
            tracing::warn!(
                source = %extractor.source.name,
                source_version = extractor.source.version,
                output = %extractor.schema.name,
                output_version = extractor.schema.version,
                stage = "declaration",
                "Find extractor is not admitted by its declared schema/source contract",
            );
            return Err(extractor_publication_failure(
                Some(extractor),
                None,
                "declaration",
            ));
        }
        by_source
            .entry(extractor.source.clone())
            .or_default()
            .push(extractor);
    }

    let mut builder =
        crate::corpus::CorpusBuilder::new(id, crate::corpus::Limits::default(), snapshot.clone());
    const EXTRACTION_PAGE: usize = 1024;
    for (source, matching) in by_source {
        let mut after: Option<BodyKey> = None;
        loop {
            let page = <SnapshotReader as crate::world::BodyReader>::body_keys_page_with_schema(
                &reader,
                world_id,
                &source.name,
                after.as_ref(),
                EXTRACTION_PAGE,
            );
            if page.is_empty() {
                break;
            }
            for body in &page {
                let Some(binding) = snapshot.binding(body) else {
                    tracing::warn!(
                        body = ?body,
                        source = %source.name,
                        source_version = source.version,
                        stage = "source-binding",
                        "Find source directory names a Body without a binding",
                    );
                    return Err(extractor_publication_failure(
                        matching.first().copied(),
                        Some(body),
                        "source-binding",
                    ));
                };
                if binding.schema != source.name {
                    tracing::warn!(
                        body = ?body,
                        source = %source.name,
                        source_version = source.version,
                        actual_schema = %binding.schema,
                        actual_version = binding.schema_version,
                        stage = "source-binding",
                        "Find source directory and Body binding disagree",
                    );
                    return Err(extractor_publication_failure(
                        matching.first().copied(),
                        Some(body),
                        "source-binding",
                    ));
                }
                if binding.schema_version != source.version {
                    continue;
                }
                let Some(stamp) =
                    <SnapshotReader as crate::world::BodyReader>::body_stamp(&reader, body)
                else {
                    tracing::warn!(
                        body = ?body,
                        source = %source.name,
                        source_version = source.version,
                        stage = "source-stamp",
                        "Find source Body has no exact publication stamp",
                    );
                    return Err(extractor_publication_failure(
                        matching.first().copied(),
                        Some(body),
                        "source-stamp",
                    ));
                };
                let Some(source_bytes) = snapshot.body_payload_bytes(body) else {
                    tracing::warn!(
                        body = ?body,
                        source = %source.name,
                        source_version = source.version,
                        stage = "source-size",
                        "Find source Body has no authenticated payload size",
                    );
                    return Err(extractor_publication_failure(
                        matching.first().copied(),
                        Some(body),
                        "source-size",
                    ));
                };
                let mut combined = crate::find::BodyExtraction {
                    body: body.clone(),
                    stamp,
                    nodes: Vec::new(),
                };
                for extractor in &matching {
                    let extracted = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        world.extract(&context, extractor, body)
                    }));
                    let mut output = match extracted {
                        Err(_) => {
                            tracing::warn!(
                                body = ?body,
                                source = %extractor.source.name,
                                source_version = extractor.source.version,
                                output = %extractor.schema.name,
                                output_version = extractor.schema.version,
                                stage = "callback-panic",
                                "Find extractor failed",
                            );
                            return Err(extractor_publication_failure(
                                Some(extractor),
                                Some(body),
                                "callback-panic",
                            ));
                        }
                        Ok(Err(failure)) => {
                            tracing::warn!(
                                body = ?body,
                                source = %extractor.source.name,
                                source_version = extractor.source.version,
                                output = %extractor.schema.name,
                                output_version = extractor.schema.version,
                                stage = "callback-rejection",
                                ?failure,
                                "Find extractor failed",
                            );
                            return Err(extractor_rejection_publication_failure(
                                extractor, body, failure,
                            ));
                        }
                        Ok(Ok(output)) => output,
                    };
                    let wrong_body = output.body != *body;
                    let wrong_stamp = output.stamp != combined.stamp;
                    let wrong_schema = output
                        .nodes
                        .iter()
                        .any(|node| node.key.schema != extractor.schema);
                    let shape_refused = !extractor.shape.admits(source_bytes, &output);
                    if wrong_body || wrong_stamp || wrong_schema || shape_refused {
                        let failure_stage = if wrong_body {
                            "output-body"
                        } else if wrong_stamp {
                            "output-stamp"
                        } else if wrong_schema {
                            "output-schema"
                        } else {
                            "output-shape"
                        };
                        tracing::warn!(
                            body = ?body,
                            source = %extractor.source.name,
                            source_version = extractor.source.version,
                            output = %extractor.schema.name,
                            output_version = extractor.schema.version,
                            wrong_body,
                            wrong_stamp,
                            wrong_schema,
                            shape_refused,
                            source_bytes,
                            output_nodes = output.nodes.len(),
                            declared_shape = ?extractor.shape,
                            stage = "output-validation",
                            "Find extractor output violated its exact contract",
                        );
                        return Err(extractor_publication_failure(
                            Some(extractor),
                            Some(body),
                            failure_stage,
                        ));
                    }
                    combined.nodes.append(&mut output.nodes);
                }
                builder
                    .push(combined)
                    .map_err(|_| PublicationFailure::Corpus)?;
            }
            after = page.last().cloned();
            if page.len() < EXTRACTION_PAGE {
                break;
            }
        }
    }
    Ok(builder.finish().map_err(|_| PublicationFailure::Corpus)?.0)
}

#[allow(clippy::too_many_arguments)]
fn candidate_world_publication(
    snapshot: Arc<replica::ReadSnapshot>,
    body_images: Arc<crate::body_image::BodyImageCache>,
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
            body_images.clone(),
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
                snapshot: snapshot.clone(),
                bodies,
            })
            .map(|(corpus, _)| corpus)
            .map_err(|_| PublicationFailure::Corpus)?
    } else {
        build_world_corpus(
            &snapshot,
            body_images,
            world,
            world_id,
            id,
            schemas,
            extractors,
        )?
    };
    Ok(Arc::new(WorldPublication {
        id,
        snapshot,
        corpus: Arc::new(corpus),
    }))
}

fn extract_changed_bodies(
    snapshot: &Arc<replica::ReadSnapshot>,
    body_images: Arc<crate::body_image::BodyImageCache>,
    prior: &crate::corpus::Corpus,
    id: crate::publication::WorldPublicationId,
    world: &Arc<dyn World>,
    world_id: &WorldId,
    schemas: &[crate::find::Schema],
    extractors: &[crate::find::Extractor],
    changed: &[BodyKey],
) -> Result<Vec<crate::find::BodyExtraction>, PublicationFailure> {
    let reader = SnapshotReader::streaming(snapshot.clone(), body_images);
    let context = crate::world::ExtractionContext::new(&reader, world_id, id);
    let mut outputs = Vec::new();
    for body in changed.iter().filter(|body| &body.world == world_id) {
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
        // Protected Runtime Bodies and product Bodies outside this Find
        // contract are not Corpus sources. Skip them before asking the public
        // reader for a stamp; the reader deliberately refuses protected
        // bindings. A Body present in the prior Corpus still flows through as
        // an empty-node replacement so a schema/source change removes its old
        // index rows.
        if matching.is_empty() && prior.body_stamp(body).is_none() {
            continue;
        }
        let stamp = match binding {
            Some(_) => <SnapshotReader as crate::world::BodyReader>::body_stamp(&reader, body)
                .ok_or_else(|| extractor_publication_failure(None, Some(body), "source-stamp"))?,
            // An absent changed Body is a real deletion. The empty stamp is
            // the Corpus delta's tombstone coordinate, not a metadata fallback.
            None => Vec::new(),
        };
        let source_bytes = match binding {
            Some(_) => snapshot
                .body_payload_bytes(body)
                .ok_or_else(|| extractor_publication_failure(None, Some(body), "source-size"))?,
            None => 0,
        };
        let mut combined = crate::find::BodyExtraction {
            body: body.clone(),
            stamp,
            nodes: Vec::new(),
        };
        for extractor in matching {
            if !schemas.iter().any(|schema| {
                schema.reference == extractor.schema && schema.sources.contains(&extractor.source)
            }) {
                tracing::warn!(
                    body = ?body,
                    source = %extractor.source.name,
                    source_version = extractor.source.version,
                    output = %extractor.schema.name,
                    output_version = extractor.schema.version,
                    stage = "declaration",
                    "changed-Body Find extractor is not admitted by its schema/source contract",
                );
                return Err(extractor_publication_failure(
                    Some(extractor),
                    Some(body),
                    "declaration",
                ));
            }
            let extracted = std::panic::catch_unwind(AssertUnwindSafe(|| {
                world.extract(&context, extractor, body)
            }));
            let mut output = match extracted {
                Err(_) => {
                    tracing::warn!(
                        body = ?body,
                        source = %extractor.source.name,
                        source_version = extractor.source.version,
                        output = %extractor.schema.name,
                        output_version = extractor.schema.version,
                        stage = "callback-panic",
                        "changed-Body Find extractor failed",
                    );
                    return Err(extractor_publication_failure(
                        Some(extractor),
                        Some(body),
                        "callback-panic",
                    ));
                }
                Ok(Err(failure)) => {
                    tracing::warn!(
                        body = ?body,
                        source = %extractor.source.name,
                        source_version = extractor.source.version,
                        output = %extractor.schema.name,
                        output_version = extractor.schema.version,
                        stage = "callback-rejection",
                        ?failure,
                        "changed-Body Find extractor failed",
                    );
                    return Err(extractor_rejection_publication_failure(
                        extractor, body, failure,
                    ));
                }
                Ok(Ok(output)) => output,
            };
            let wrong_body = output.body != *body;
            let wrong_stamp = output.stamp != combined.stamp;
            let wrong_schema = output
                .nodes
                .iter()
                .any(|node| node.key.schema != extractor.schema);
            let shape_refused = !extractor.shape.admits(source_bytes, &output);
            if wrong_body || wrong_stamp || wrong_schema || shape_refused {
                let failure_stage = if wrong_body {
                    "output-body"
                } else if wrong_stamp {
                    "output-stamp"
                } else if wrong_schema {
                    "output-schema"
                } else {
                    "output-shape"
                };
                tracing::warn!(
                    body = ?body,
                    source = %extractor.source.name,
                    source_version = extractor.source.version,
                    output = %extractor.schema.name,
                    output_version = extractor.schema.version,
                    wrong_body,
                    wrong_stamp,
                    wrong_schema,
                    shape_refused,
                    source_bytes,
                    output_nodes = output.nodes.len(),
                    declared_shape = ?extractor.shape,
                    stage = "output-validation",
                    "changed-Body Find extractor output violated its exact contract",
                );
                return Err(extractor_publication_failure(
                    Some(extractor),
                    Some(body),
                    failure_stage,
                ));
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
        snapshot: &replica::ReadSnapshot,
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
                    || snapshot
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
            let (schema_id, version) = if let Some(existing) = snapshot.binding(key) {
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
            let permitted = operation_matches_mutation_model(&schema.mutation, op);
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
                        mutation_model: mutation_model_tag(&schema.mutation),
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
            let run = binding.schema.as_str() == crate::exec::RUN_BODY_SCHEMA
                && binding.schema_version == crate::exec::RUN_BODY_SCHEMA_VERSION
                && binding.mutation_model == replica::body::MUTATION_COLLABORATIVE;
            let active = binding.schema.as_str() == crate::exec::ACTIVE_RUN_BODY_SCHEMA
                && binding.schema_version == crate::exec::ACTIVE_RUN_BODY_SCHEMA_VERSION
                && binding.mutation_model == replica::body::MUTATION_ATOMIC;
            if key.world != self.world_id
                || (!run && !active)
                || binding.encoding.as_str() != crate::exec::BODY_ENCODING
                || snapshot
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

    /// Enforce the nonzero, registration-validated World payload ceiling
    /// before any product decoder or callback observes attacker-owned bytes.
    fn ensure_within_limit(&self, payload_len: usize) -> Result<(), Rejection> {
        let max = self.limits.max_payload_bytes;
        if payload_len > max as usize {
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
        let inner = self.core.lock();
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
        drop(inner);
        let operation = crate::action::RequestId::mint().as_bytes();
        let digest = *blake3::hash(&build.id.as_bytes()).as_bytes();
        self.commit_runtime_effect(
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

    fn receipt_semantic(
        receipt: &replica::receipt::RequestReceipt,
    ) -> crate::publication::PublicationId {
        crate::publication::PublicationId::new(
            receipt.manifest_root,
            receipt.implementation_digest,
            crate::publication::ExtractorSchemaDigest::from_digest(receipt.extractor_schema_digest),
        )
    }

    fn receipt_readiness(
        &self,
        receipt: &replica::receipt::RequestReceipt,
    ) -> OperationPublication {
        let semantic = Self::receipt_semantic(receipt);
        self.semantic_readiness(&receipt.world, semantic)
    }

    fn semantic_readiness(
        &self,
        world_id: &WorldId,
        semantic: crate::publication::PublicationId,
    ) -> OperationPublication {
        let builder = match (
            self.registry
                .world_for(world_id, semantic.implementation_digest),
            self.registry
                .descriptor_for(world_id, semantic.implementation_digest),
        ) {
            (Some(world), Some(descriptor)) => {
                let Ok(digest) = crate::publication::ExtractorSchemaDigest::derive(
                    &descriptor.find_schemas,
                    &descriptor.find_extractors,
                ) else {
                    return OperationPublication::ImplementationUnavailable;
                };
                if digest != semantic.extractor_schema_digest {
                    return OperationPublication::ImplementationUnavailable;
                }
                WorldPublicationBuilder {
                    world,
                    implementation: semantic.implementation_digest,
                    extractor_schema_digest: digest,
                    schemas: descriptor.find_schemas.clone(),
                    extractors: descriptor.find_extractors.clone(),
                }
            }
            _ if semantic.implementation_digest == self.implementation
                && semantic.extractor_schema_digest == self.extractor_schema_digest =>
            {
                // Explicit unreviewed embedder mode remains bound to exactly
                // the Session contract; it never resolves an arbitrary digest.
                WorldPublicationBuilder {
                    world: self.world.clone(),
                    implementation: self.implementation,
                    extractor_schema_digest: self.extractor_schema_digest,
                    schemas: self.world.find_schemas().to_vec(),
                    extractors: self.world.find_extractors().to_vec(),
                }
            }
            _ => return OperationPublication::ImplementationUnavailable,
        };
        self.core
            .schedule_receipt_publication(world_id.clone(), semantic, builder, false)
    }

    fn operation_status_from_receipt(
        &self,
        receipt: replica::receipt::RequestReceipt,
    ) -> OperationStatus {
        let publication = self.receipt_readiness(&receipt);
        let semantic = Self::receipt_semantic(&receipt);
        OperationStatus::Found {
            receipt: DurableOperationReceipt {
                operation: receipt.request,
                payload_hash: receipt.payload_hash,
                effect: receipt.effect,
                frontier: receipt.frontier,
                bodies: receipt.bodies,
                publication: semantic,
                transaction: receipt.transaction,
            },
            publication,
        }
    }

    /// Pin and query the authoritative receipt index without keeping either
    /// mutable Runtime lock across a cold journal/index/object read.
    fn check_action_readonly(
        &self,
        device: &mechanics::ids::DeviceId,
        operation: &[u8; 16],
        payload_hash: &[u8; 32],
    ) -> Result<ActionReceiptCheck, Failure> {
        let reader = self.core.replica_lock().receipt_reader();
        let result = if let Some(reader) = reader {
            let footprint = reader.footprint();
            let (read_memory, station) = {
                let mut inner = self.core.lock();
                // A cold authenticated receipt lookup is bounded transient
                // work, not a reason to strand reconstructable publication
                // caches at the Station ceiling. Reclaim those caches before
                // asking the shared governor for the declared lookup peak;
                // current, cursor, and deferred publications remain pinned.
                inner
                    .make_transient_read_room(footprint.cold_lookup_transient_upper_bound)
                    .map_err(|_| Failure::ReadCapacity)?;
                (inner.read_memory.clone(), inner.station_memory.station)
            };
            let _lookup_memory = read_memory
                .reserve_build(station, footprint.cold_lookup_transient_upper_bound)
                .map_err(|_| Failure::ReadCapacity)?;
            reader
                .check_action(&self.space, &self.world_id, device, operation, payload_hash)
                .map(|check| match check {
                    replica::ReceiptCheck::Replayed(receipt) => {
                        ActionReceiptCheck::Replayed(receipt)
                    }
                    replica::ReceiptCheck::Absent(absence) => {
                        ActionReceiptCheck::DurableAbsent(absence)
                    }
                })
        } else {
            // Scratch/non-durable Replicas retain their tiny receipt directory
            // in memory and have no journal reader to detach.
            self.core
                .replica_lock()
                .lookup_action(&self.space, &self.world_id, device, operation, payload_hash)
                .map(|receipt| {
                    receipt.map_or(
                        ActionReceiptCheck::ScratchAbsent,
                        ActionReceiptCheck::Replayed,
                    )
                })
        };
        result.map_err(|failure| match failure {
            replica::transaction::commit::Failure::RequestIdConflict => {
                Failure::Conflict(Conflict::Request)
            }
            other => commit_failure(other),
        })
    }

    fn lookup_action_readonly(
        &self,
        device: &mechanics::ids::DeviceId,
        operation: &[u8; 16],
        payload_hash: &[u8; 32],
    ) -> Result<Option<replica::receipt::RequestReceipt>, Failure> {
        self.check_action_readonly(device, operation, payload_hash)
            .map(|check| match check {
                ActionReceiptCheck::Replayed(receipt) => Some(receipt),
                ActionReceiptCheck::DurableAbsent(_) | ActionReceiptCheck::ScratchAbsent => None,
            })
    }

    fn committed_effect_from_ready_receipt(
        &self,
        receipt: replica::receipt::RequestReceipt,
    ) -> Result<CommittedEffect, Failure> {
        let publication = match self.receipt_readiness(&receipt) {
            OperationPublication::Ready(publication) => publication,
            // Durability is known but the exact renderable read atom is not
            // installed yet. Returning a capacity/retry answer here would let
            // an acknowledgement path treat a committed operation as though
            // it had not happened; reconcile through `operation_status`.
            OperationPublication::Building => return Err(Failure::OutcomeUnknown),
            OperationPublication::Capacity => return Err(Failure::ReadCapacity),
            OperationPublication::ImplementationUnavailable => {
                return Err(Rejection::ImplementationUnavailable.into());
            }
            OperationPublication::GenerationUnavailable | OperationPublication::Unavailable => {
                return Err(Failure::GenerationUnavailable);
            }
        };
        Ok(CommittedEffect {
            operation: receipt.request,
            effect: receipt.effect,
            frontier: receipt.frontier,
            bodies: receipt.bodies,
            publication,
        })
    }

    /// Read one persistent idempotency coordinate without invoking World code
    /// or committing when it is absent.
    ///
    /// A durable cache miss may perform bounded index/object I/O. Callers on an
    /// async reactor must therefore enter this synchronous API through the
    /// composition-owned bounded blocking lane. Runtime never holds the Core
    /// publication mutex during that I/O. `Found` remains authoritative even
    /// when its semantic publication is not locally Ready yet.
    pub fn operation_status(
        &self,
        operation: [u8; 16],
        payload_hash: [u8; 32],
    ) -> Result<OperationStatus, Failure> {
        self.ensure_live()?;
        let principal = self.fresh_principal()?;
        let receipt = self.lookup_action_readonly(&principal.device, &operation, &payload_hash)?;
        Ok(receipt.map_or(OperationStatus::Absent, |receipt| {
            self.operation_status_from_receipt(receipt)
        }))
    }

    /// Reconcile a submitted World intent without asking the caller to
    /// reproduce Runtime's signed-action payload commitment.
    pub fn operation_status_for(
        &self,
        operation: crate::action::RequestId,
        intent: &Intent,
    ) -> Result<OperationStatus, Failure> {
        self.operation_status(operation.as_bytes(), intent.payload_hash())
    }

    /// Resolve or start rebuilding one portable publication for a bounded
    /// lifecycle planning step. This never waits for extraction: `Building`
    /// is returned after installing/joining the existing bounded singleflight.
    pub fn lifecycle_source_status(
        &self,
        source: Option<crate::publication::PublicationId>,
    ) -> Result<LifecycleSourceStatus, Failure> {
        self.ensure_live()?;
        let semantic = if let Some(source) = source {
            source
        } else {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            let publication = inner
                .world_publications
                .get(&self.world_id)
                .ok_or(Rejection::ImplementationUnavailable)?;
            publication.id.publication
        };
        let readiness = self.semantic_readiness(&self.world_id, semantic);
        let status = match readiness {
            OperationPublication::Ready(id) => {
                let publication = self
                    .core
                    .lock()
                    .exact_world_publication(&(self.world_id.clone(), id));
                match publication {
                    Some(publication) => LifecycleSourceStatus::Ready(LifecycleSourceCoordinate {
                        publication: publication.id,
                        frontier: publication.snapshot.frontier(),
                    }),
                    None => LifecycleSourceStatus::Building,
                }
            }
            OperationPublication::Building => LifecycleSourceStatus::Building,
            OperationPublication::Capacity => LifecycleSourceStatus::Capacity,
            OperationPublication::ImplementationUnavailable => {
                LifecycleSourceStatus::ImplementationUnavailable
            }
            OperationPublication::GenerationUnavailable => {
                LifecycleSourceStatus::GenerationUnavailable
            }
            OperationPublication::Unavailable => LifecycleSourceStatus::Unavailable,
        };
        Ok(status)
    }

    /// Run one product-owned, read-only migration planner over an exact frozen
    /// publication. The publication Arc is pinned for the callback and no
    /// Station/Replica/mutation lock is held while product code runs.
    ///
    /// The callback's return type is unconstrained so product diagnostics stay
    /// product-owned; Runtime only contains panics and exact-source expiry.
    pub fn with_lifecycle_source<T>(
        &self,
        source: &LifecycleSourceCoordinate,
        prepare: impl FnOnce(&Context<'_>) -> T,
    ) -> Result<T, Failure> {
        self.ensure_live()?;
        let principal = self.fresh_principal()?;
        let publication = {
            let mut inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            inner
                .exact_world_publication(&(self.world_id.clone(), source.publication))
                .ok_or(Failure::PublicationExpired(source.publication))?
        };
        if publication.snapshot.frontier() != source.frontier {
            return Err(Rejection::ContractViolation.into());
        }
        let descriptor = self
            .registry
            .descriptor_for(
                &self.world_id,
                publication.id.publication.implementation_digest,
            )
            .ok_or(Rejection::ImplementationUnavailable)?;
        let gates = self.context_find_gates_for(&principal, &descriptor.find_schemas)?;
        let reader = SnapshotReader::interactive(
            publication.snapshot.clone(),
            self.core.body_images.clone(),
        );
        let (read_memory, station_memory, publication_retention, admitted_retained_bytes) = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            (
                inner.read_memory.clone(),
                inner.station_memory.clone(),
                inner.publication_retention.clone(),
                inner
                    .station_read_retained_bytes()
                    .saturating_add(inner.publication_incremental_bytes(&publication)),
            )
        };
        let issued_find_cursor = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let find = crate::world::FindHandle::new(Arc::new(ContextFindReader {
            read_memory,
            station_memory,
            publication_retention,
            admitted_retained_bytes,
            publication: publication.clone(),
            schemas: Arc::from(descriptor.find_schemas.clone()),
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
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let context = Context::with_lifecycle_reads(
                &principal,
                &reader,
                source.clone(),
                &self.world_id,
                find,
            );
            prepare(&context)
        }))
        .map_err(|_| Failure::CallbackPanicked)?;
        if issued_find_cursor.load(std::sync::atomic::Ordering::Acquire) {
            self.core
                .lock()
                .lease_world_publication(self.world_id.clone(), publication)
                .map_err(|_| Failure::ReadCapacity)?;
        }
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) fn test_building_memory_bytes(&self) -> u64 {
        let inner = self.core.lock();
        let bytes = inner
            .read_memory
            .state
            .lock_recovering()
            .get(&inner.station_memory.station)
            .map_or(0, |account| account.building);
        bytes
    }

    #[cfg(test)]
    pub(crate) fn evict_semantic_publication_for_test(
        &self,
        semantic: crate::publication::PublicationId,
    ) {
        let mut inner = self.core.lock();
        inner
            .retained_world_publications
            .retain(|(_, id), _| id.publication != semantic);
        inner
            .world_publication_order
            .retain(|(_, id)| id.publication != semantic);
        inner
            .world_read_heads
            .retain(|(_, id), _| id.publication != semantic);
        let _ = inner.sync_read_memory();
    }

    #[cfg(test)]
    pub(crate) fn evict_generation_for_test(&self, root: [u8; 32]) {
        let mut inner = self.core.lock();
        assert_ne!(
            root,
            inner.snapshot.root(),
            "the current generation is authoritative"
        );
        inner.generations.remove(&root);
        inner.parents.remove(&root);
        inner
            .generation_order
            .retain(|candidate| candidate != &root);
        inner
            .retained_world_publications
            .retain(|(_, id), _| id.publication.manifest_root != root);
        inner
            .world_publication_order
            .retain(|(_, id)| id.publication.manifest_root != root);
        inner
            .world_read_heads
            .retain(|(_, id), _| id.publication.manifest_root != root);
        let _ = inner.sync_read_memory();
    }

    #[cfg(test)]
    pub(crate) fn generation_index_root_for_test(&self) -> Option<[u8; 32]> {
        let current = { self.core.lock().snapshot.clone() };
        let reader = { self.core.replica_lock().generation_reader(current) };
        reader.generation_index_root_for_test()
    }

    #[cfg(test)]
    pub(crate) fn mark_semantic_publication_building_for_test(
        &self,
        semantic: crate::publication::PublicationId,
    ) {
        self.evict_semantic_publication_for_test(semantic);
        let mut inner = self.core.lock();
        let materialization = if semantic.manifest_root == inner.snapshot.root() {
            inner.snapshot_materialization
        } else {
            inner
                .generations
                .get(&semantic.manifest_root)
                .expect("test semantic generation")
                .materialization
        };
        inner.world_read_heads.insert(
            (
                self.world_id.clone(),
                crate::publication::WorldPublicationId::new(semantic, materialization),
            ),
            WorldReadHead::Building,
        );
    }

    #[cfg(test)]
    pub(crate) fn mark_semantic_publication_capacity_for_test(
        &self,
        semantic: crate::publication::PublicationId,
    ) {
        self.evict_semantic_publication_for_test(semantic);
        let mut inner = self.core.lock();
        let materialization = if semantic.manifest_root == inner.snapshot.root() {
            inner.snapshot_materialization
        } else {
            inner
                .generations
                .get(&semantic.manifest_root)
                .expect("test semantic generation")
                .materialization
        };
        inner.world_read_heads.insert(
            (
                self.world_id.clone(),
                crate::publication::WorldPublicationId::new(semantic, materialization),
            ),
            WorldReadHead::Unavailable(PublicationFailure::Capacity),
        );
    }

    #[cfg(test)]
    pub(crate) fn test_receipt_implementation_readiness(
        &self,
        operation: [u8; 16],
        payload_hash: [u8; 32],
        implementation: [u8; 32],
    ) -> OperationPublication {
        let principal = self.fresh_principal().expect("test principal");
        let mut receipt = self
            .lookup_action_readonly(&principal.device, &operation, &payload_hash)
            .expect("test receipt lookup")
            .expect("test durable receipt");
        receipt.implementation_digest = implementation;
        self.receipt_readiness(&receipt)
    }

    #[cfg(test)]
    pub(crate) fn test_read_reserved_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<crate::world::CollaborativeBody, crate::world::BodyReadFailure> {
        let inner = self.core.lock();
        let snapshot = inner.snapshot.clone();
        drop(inner);
        let reader = SnapshotReader::interactive(snapshot, self.core.body_images.clone());
        reader.read_reserved_collaborative(key)?.ok_or_else(|| {
            crate::world::BodyReadFailure::MaterialUnavailable(
                crate::world::BodyReadCoordinate::new(key.clone(), None),
            )
        })
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
        self.submit_with_provenance(action, None)
    }

    /// Submit one deterministically prepared lifecycle action against current
    /// durable truth while carrying its exact frozen planning source as
    /// non-serializable provenance. A raw World call cannot manufacture that
    /// source coordinate in signed intent bytes.
    pub fn submit_lifecycle_from(
        &self,
        action: crate::action::SignedWorldAction,
        source: LifecycleSourceCoordinate,
    ) -> Result<CommittedEffect, Failure> {
        self.submit_with_provenance(action, Some(source))
    }

    fn submit_with_provenance(
        &self,
        action: crate::action::SignedWorldAction,
        lifecycle_source: Option<LifecycleSourceCoordinate>,
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
        // Resolve the principal and pin one immutable publication first. The
        // World callback runs on that Arc with neither Station mutex held;
        // Find, Live, and other projections therefore remain responsive even
        // when domain validation is slow. Replica is touched only briefly for
        // the idempotency lookup, in the global Replica -> Core lock order.
        let principal = self.fresh_principal()?;
        if action.header.actor != principal.actor || action.header.device != principal.device {
            return Err(Rejection::Denied(DeniedCause::PrincipalMismatch).into());
        }
        let receipt_absence =
            match self.check_action_readonly(&principal.device, &request, &payload_hash)? {
                ActionReceiptCheck::Replayed(receipt) => {
                    return self.committed_effect_from_ready_receipt(receipt);
                }
                ActionReceiptCheck::DurableAbsent(absence) => Some(absence),
                ActionReceiptCheck::ScratchAbsent => None,
            };
        let find_gates = self.context_find_gates(&principal)?;
        let (
            publication,
            pinned,
            ambient,
            read_memory,
            station_memory,
            publication_retention,
            admitted_retained_bytes,
            lifecycle_source_publication,
        ) = {
            let mut inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
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
            let publication = ready_inner_world_publication(
                &inner,
                &self.world_id,
                ambient.implementation,
                ambient.extractor_schema_digest,
            )
            .map_err(|failure| {
                submission_publication_failure(failure, "pin World submission publication")
            })?;
            let pinned = publication.snapshot.clone();
            let admitted_retained_bytes = inner
                .station_read_retained_bytes()
                .saturating_add(inner.publication_incremental_bytes(&publication));
            let lifecycle_source_publication = if let Some(source) = &lifecycle_source {
                let source_publication = inner
                    .exact_world_publication(&(self.world_id.clone(), source.publication))
                    .ok_or(Failure::PublicationExpired(source.publication))?;
                if source_publication.snapshot.frontier() != source.frontier {
                    return Err(Rejection::ContractViolation.into());
                }
                Some(source_publication)
            } else {
                None
            };
            (
                publication,
                pinned,
                ambient,
                inner.read_memory.clone(),
                inner.station_memory.clone(),
                inner.publication_retention.clone(),
                admitted_retained_bytes,
                lifecycle_source_publication,
            )
        };
        // Admission precedes the potentially size-proportional World callback
        // and candidate extraction. A competing human, agent, or Contact
        // operation gets `Busy`/`Capacity` immediately; it never waits behind
        // this synchronous API with no operation-phase feedback.
        let _mutation = self.core.try_mutation_lane().map_err(commit_failure)?;
        // A winner may have committed after the optimistic lookup but before
        // this request acquired mutation admission. Recheck the authoritative
        // receipt index now, before entering potentially expensive World code;
        // the later pre-prepare check remains the final idempotency guard.
        match self.lookup_action_readonly(&principal.device, &request, &payload_hash)? {
            None => {}
            Some(receipt) => return self.committed_effect_from_ready_receipt(receipt),
        }
        let issued_find_cursor = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = SnapshotReader::interactive(pinned.clone(), self.core.body_images.clone());
        let lifecycle_reader = lifecycle_source_publication.as_ref().map(|publication| {
            SnapshotReader::interactive(publication.snapshot.clone(), self.core.body_images.clone())
        });
        let effect: Effect = {
            let principal = &principal;
            let find = crate::world::FindHandle::new(Arc::new(ContextFindReader {
                read_memory,
                station_memory,
                publication_retention,
                admitted_retained_bytes,
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
                    lifecycle_reader
                        .as_ref()
                        .map(|reader| reader as &dyn crate::world::BodyReader),
                    publication.id,
                    &self.world_id,
                    action.header.request,
                    find,
                    lifecycle_source.clone(),
                );
                world.submit(&mut ctx, intent)
            }))
            .map_err(|_| Failure::CallbackPanicked)?;
            decision.map_err(Failure::Rejected)?
        };
        // A cursor minted inside a SUBMIT callback is not retained, and the
        // difference from the query path above is the whole reason: a query
        // hands its cursor back to the caller, who may present it later, so
        // the publication it names has to outlive the request. A submission
        // answers with an effect. Its cursor reaches nobody, and paging
        // WITHIN the callback is already served by the publication the
        // callback holds pinned for its own duration.
        //
        // Retaining it anyway had a cost that fell on writes: leases are
        // capped, each commit mints a fresh publication so every lease is a
        // new key rather than a refresh, and a World that reads a page before
        // it writes mints a continuation on every call. Enough writes inside
        // one lease window and the next one is refused for the sake of a
        // continuation nobody can present.
        let runtime = {
            let now_millis = mechanics::wallclock::now_millis();
            let offer_guard = self.core.lock();
            let admission = OfferAdmission {
                news: &offer_guard.offers,
                readies: &offer_guard.readies,
                now_millis,
            };
            lower_exec(
                &effect.exec,
                world.exec_specs(),
                &ambient,
                request,
                effect.operations.len(),
                &reader,
                &admission,
            )
            .inspect_err(|rejection| {
                tracing::warn!(?rejection, "Runtime refused a staged Exec command");
            })?
        };
        // Contain the staged effect inside this World's namespace and each
        // Body's exact schema binding, resolving the bindings the commit is
        // made under.
        let bindings = self
            .contain_effect(&pinned, &effect, &intent_schema, &runtime)
            .inspect_err(|rejection| {
                tracing::warn!(?rejection, "World effect containment failed");
            })?;
        // Re-resolve authority after the callback, then enter the one Replica
        // mutation lane. The pinned parent is compared again under that lane;
        // another local or Contact commit never gets reinterpreted as this
        // action's base.
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
        let mut replica = self.core.replica_lock();
        let (prior_publication, prior, candidate_materialization, build_governor, build_station) = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            if inner.snapshot.root() != pinned.root()
                || inner.snapshot_materialization != publication.id.materialization
            {
                return Err(Failure::Conflict(Conflict::Body));
            }
            (
                inner.world_publications.get(&self.world_id).cloned(),
                inner.snapshot.clone(),
                inner.next_materialization,
                inner.read_memory.clone(),
                inner.station_memory.station,
            )
        };
        if StationCore::current_replica_root(&replica) != prior.root() {
            return Err(Failure::Conflict(Conflict::Body));
        }
        let parent_manifest_root = replica.manifest_root();
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
        let prepare = |replica: &mut replica::Replica, absence: Option<replica::ReceiptAbsence>| {
            let interpretation = replica::receipt::Interpretation {
                implementation_digest: self.implementation,
                extractor_schema_digest: self.extractor_schema_digest.digest(),
            };
            if let Some(absence) = absence {
                replica.prepare_action_checked(
                    &ctx,
                    &auth,
                    interpretation,
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
                    absence,
                )
            } else {
                replica.prepare_action(
                    &ctx,
                    &auth,
                    interpretation,
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
            }
        };
        let outcome = prepare(&mut replica, receipt_absence).map_err(commit_failure)?;
        // `PreparedAction` owns the candidate independently of Replica. Keep
        // the outer try-admitted mutation permit, but release the durable
        // writer through every size-proportional projection/build step.
        let mut replica = Some(replica);
        let (receipt, fresh) = match outcome {
            replica::transaction::PreparedActionOutcome::Replayed(receipt) => {
                drop(replica.take());
                return self.committed_effect_from_ready_receipt(receipt);
            }
            replica::transaction::PreparedActionOutcome::Prepared(prepared) => {
                let candidate_receipt = prepared.receipt().map_err(commit_failure)?.clone();
                drop(replica.take());
                let snapshot_delta_bytes = prepared
                    .candidate_snapshot_delta_bytes_estimate(&prior)
                    .map_err(commit_failure)?;
                self.core
                    .lock()
                    .make_read_room(snapshot_delta_bytes)
                    .map_err(|_| Failure::ReadCapacity)?;
                let mut build_memory = build_governor
                    .reserve_build(build_station, snapshot_delta_bytes)
                    .map_err(|_| Failure::ReadCapacity)?;
                let snapshot = Arc::new(
                    prepared
                        .candidate_snapshot(&prior)
                        .map_err(commit_failure)?,
                );
                let snapshot_bytes = snapshot.retained_bytes_estimate();
                let corpus_memory = prior_publication
                    .as_ref()
                    .filter(|prior| {
                        prior.id.publication.implementation_digest == self.implementation
                            && prior.id.publication.extractor_schema_digest
                                == self.extractor_schema_digest
                    })
                    .map(|prior| {
                        prior.corpus.estimate_delta_build_bytes(
                            &snapshot,
                            &self.world_id,
                            self.world.find_extractors(),
                            &candidate_receipt.bodies,
                        )
                    })
                    .unwrap_or_else(|| {
                        crate::corpus::Corpus::estimate_build_bytes(
                            &snapshot,
                            &self.world_id,
                            self.world.find_extractors(),
                        )
                    });
                let corpus_build_bytes = corpus_memory
                    .retained_bytes
                    .saturating_add(corpus_memory.transient_bytes);
                self.core
                    .lock()
                    .make_read_room(snapshot_delta_bytes.saturating_add(corpus_build_bytes))
                    .map_err(|_| Failure::ReadCapacity)?;
                build_memory
                    .grow(corpus_build_bytes)
                    .map_err(|_| Failure::ReadCapacity)?;
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
                    self.core.body_images.clone(),
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
                    session_publication_failure(failure, "build candidate World publication")
                })?;
                let publication_bytes =
                    snapshot_bytes.saturating_add(publication.corpus.retained_bytes_estimate());
                self.core
                    .lock()
                    .make_read_room(publication_bytes.max(build_memory.bytes))
                    .map_err(|_| Failure::ReadCapacity)?;
                let resident = self
                    .core
                    .lock()
                    .station_read_retained_bytes()
                    .saturating_add(publication_bytes);
                let resident_transition = build_memory
                    .prepare_resident(resident)
                    .map_err(|_| Failure::ReadCapacity)?;

                // Only after the exact corpus is ready do signed material,
                // Manifest, generation, and receipt cross the journal commit
                // point. A failed build above drops `prepared` and rolls the
                // Fabric candidate back.
                let mut durable_replica = self.core.replica_lock();
                let receipt = prepared
                    .finalize_attached(&mut durable_replica, &ctx, snapshot.as_ref())
                    .map_err(commit_failure)?;
                replica = Some(durable_replica);
                debug_assert_eq!(receipt, candidate_receipt);
                resident_transition.commit();
                (receipt, Some((snapshot, publication)))
            }
        };
        let committed_publication = fresh
            .as_ref()
            .map(|(_, publication)| publication.id)
            .ok_or(Failure::Persistence)?;
        if let Some((snapshot, publication)) = fresh {
            // Publish the immutable snapshot + corpus and its Observation
            // while still holding the writer. Readers can observe the old
            // pair or the new pair, never a durable root with a stale
            // target-World corpus.
            let mut inner = self.core.lock();
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
        {
            let consumed_reader =
                SnapshotReader::interactive(pinned.clone(), self.core.body_images.clone());
            let mut inner = self.core.lock();
            consume_first_use_readies(&mut inner, &effect.exec, &consumed_reader, &ambient.world);
        }
        drop(replica);
        Ok(CommittedEffect {
            operation: receipt.request,
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
                self.fresh_principal()
                    .map_err(Failure::from)
                    .map_err(crate::exec::WorkRefusal::from)?;
                let inner = self.core.lock();
                if inner.closed {
                    return Err(crate::exec::WorkRefusal::Session(Failure::Interrupted));
                }
                let snapshot = inner.snapshot.clone();
                drop(inner);
                let reader = SnapshotReader::interactive(snapshot, self.core.body_images.clone());
                work_reply(&reader, &request)
            }
            crate::exec::WorkRequest::Cancel { .. }
            | crate::exec::WorkRequest::Retry { .. }
            | crate::exec::WorkRequest::Resume { .. } => {
                let digest = request.digest()?;
                let principal = self
                    .fresh_principal()
                    .map_err(Failure::from)
                    .map_err(crate::exec::WorkRefusal::from)?;
                // Durable receipt lookup may touch a cold index/object, so it
                // runs without the Core publication mutex. Exec validation and
                // continuation projection below likewise hold neither mutex.
                let replayed = self
                    .lookup_action_readonly(&principal.device, &operation, &digest)
                    .map_err(crate::exec::WorkRefusal::from)?
                    .is_some();
                let (ambient, pinned, pinned_materialization) = {
                    let inner = self.core.lock();
                    if inner.closed {
                        return Err(crate::exec::WorkRefusal::Session(Failure::Interrupted));
                    }
                    let ambient = if replayed {
                        None
                    } else {
                        Some(
                            self.ambient(&principal, inner.snapshot.root())
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
                                .map_err(crate::exec::WorkRefusal::from)?,
                        )
                    };
                    (
                        ambient,
                        inner.snapshot.clone(),
                        inner.snapshot_materialization,
                    )
                };
                let Some(ambient) = ambient else {
                    let reader = SnapshotReader::interactive(pinned, self.core.body_images.clone());
                    return work_reply(&reader, &request);
                };
                let _mutation = self
                    .core
                    .try_mutation_lane()
                    .map_err(commit_failure)
                    .map_err(crate::exec::WorkRefusal::from)?;
                let receipt_absence = match self
                    .check_action_readonly(&principal.device, &operation, &digest)
                    .map_err(crate::exec::WorkRefusal::from)?
                {
                    ActionReceiptCheck::Replayed(_) => {
                        let snapshot = self.core.lock().snapshot.clone();
                        let reader =
                            SnapshotReader::interactive(snapshot, self.core.body_images.clone());
                        return work_reply(&reader, &request);
                    }
                    ActionReceiptCheck::DurableAbsent(absence) => Some(absence),
                    ActionReceiptCheck::ScratchAbsent => None,
                };
                let pinned_reader =
                    SnapshotReader::interactive(pinned.clone(), self.core.body_images.clone());
                let (command, label) = match &request {
                    crate::exec::WorkRequest::Cancel { run, .. } => {
                        (crate::exec::Cmd::Cancel { run: *run }, "exec.work.cancel")
                    }
                    crate::exec::WorkRequest::Retry { .. } => (
                        crate::exec::Cmd::Try(continuation_try(
                            &pinned_reader,
                            self.world.exec_specs(),
                            &ambient,
                            &request,
                        )?),
                        "exec.work.continue",
                    ),
                    crate::exec::WorkRequest::Resume { .. } => (
                        crate::exec::Cmd::Try(continuation_try(
                            &pinned_reader,
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
                let runtime = {
                    let now_millis = mechanics::wallclock::now_millis();
                    let offer_guard = self.core.lock();
                    let admission = OfferAdmission {
                        news: &offer_guard.offers,
                        readies: &offer_guard.readies,
                        now_millis,
                    };
                    lower_exec(
                        std::slice::from_ref(&command),
                        self.world.exec_specs(),
                        &ambient,
                        operation,
                        0,
                        &pinned_reader,
                        &admission,
                    )
                    .map_err(Failure::from)
                    .map_err(crate::exec::WorkRefusal::from)?
                };
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
                // Serialize only the durable mutation from this point. The
                // exact pinned parent is validated after entering the lane;
                // no stale continuation is silently replayed on a newer Run
                // head. Core is borrowed only for this validation/capture and
                // the final immutable publication install.
                let mut replica = self.core.replica_lock();
                let (
                    prior_publication,
                    prior,
                    candidate_materialization,
                    build_governor,
                    build_station,
                ) = {
                    let inner = self.core.lock();
                    if inner.closed {
                        return Err(crate::exec::WorkRefusal::Session(Failure::Interrupted));
                    }
                    if inner.snapshot.root() != pinned.root()
                        || inner.snapshot_materialization != pinned_materialization
                        || StationCore::current_replica_root(&replica) != pinned.root()
                    {
                        return Err(crate::exec::WorkRefusal::Session(Failure::Conflict(
                            Conflict::Body,
                        )));
                    }
                    (
                        inner.world_publications.get(&self.world_id).cloned(),
                        inner.snapshot.clone(),
                        inner.next_materialization,
                        inner.read_memory.clone(),
                        inner.station_memory.station,
                    )
                };
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
                let interpretation = replica::receipt::Interpretation {
                    implementation_digest: self.implementation,
                    extractor_schema_digest: self.extractor_schema_digest.digest(),
                };
                let outcome = if let Some(absence) = receipt_absence {
                    replica.prepare_action_checked(
                        &commit,
                        &authorization,
                        interpretation,
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
                        absence,
                    )
                } else {
                    replica.prepare_action(
                        &commit,
                        &authorization,
                        interpretation,
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
                }
                .map_err(commit_failure)
                .map_err(crate::exec::WorkRefusal::from)?;
                let mut replica = Some(replica);
                let fresh = match outcome {
                    replica::transaction::PreparedActionOutcome::Replayed(_) => None,
                    replica::transaction::PreparedActionOutcome::Prepared(prepared) => {
                        let candidate_receipt = prepared
                            .receipt()
                            .map_err(commit_failure)
                            .map_err(crate::exec::WorkRefusal::from)?
                            .clone();
                        drop(replica.take());
                        let snapshot_delta_bytes = prepared
                            .candidate_snapshot_delta_bytes_estimate(&prior)
                            .map_err(commit_failure)
                            .map_err(crate::exec::WorkRefusal::from)?;
                        self.core
                            .lock()
                            .make_read_room(snapshot_delta_bytes)
                            .map_err(|_| {
                                crate::exec::WorkRefusal::Session(Failure::ReadCapacity)
                            })?;
                        let mut build_memory = build_governor
                            .reserve_build(build_station, snapshot_delta_bytes)
                            .map_err(|_| {
                                crate::exec::WorkRefusal::Session(Failure::ReadCapacity)
                            })?;
                        let snapshot = Arc::new(
                            prepared
                                .candidate_snapshot(&prior)
                                .map_err(commit_failure)
                                .map_err(crate::exec::WorkRefusal::from)?,
                        );
                        let snapshot_bytes = snapshot.retained_bytes_estimate();
                        let corpus_memory = prior_publication
                            .as_ref()
                            .filter(|prior| {
                                prior.id.publication.implementation_digest == self.implementation
                                    && prior.id.publication.extractor_schema_digest
                                        == self.extractor_schema_digest
                            })
                            .map(|prior| {
                                prior.corpus.estimate_delta_build_bytes(
                                    &snapshot,
                                    &self.world_id,
                                    self.world.find_extractors(),
                                    &candidate_receipt.bodies,
                                )
                            })
                            .unwrap_or_else(|| {
                                crate::corpus::Corpus::estimate_build_bytes(
                                    &snapshot,
                                    &self.world_id,
                                    self.world.find_extractors(),
                                )
                            });
                        let corpus_build_bytes = corpus_memory
                            .retained_bytes
                            .saturating_add(corpus_memory.transient_bytes);
                        self.core
                            .lock()
                            .make_read_room(snapshot_delta_bytes.saturating_add(corpus_build_bytes))
                            .map_err(|_| {
                                crate::exec::WorkRefusal::Session(Failure::ReadCapacity)
                            })?;
                        build_memory.grow(corpus_build_bytes).map_err(|_| {
                            crate::exec::WorkRefusal::Session(Failure::ReadCapacity)
                        })?;
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
                            self.core.body_images.clone(),
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
                            crate::exec::WorkRefusal::Session(session_publication_failure(
                                failure,
                                "build candidate World publication",
                            ))
                        })?;
                        let publication_bytes = snapshot_bytes
                            .saturating_add(publication.corpus.retained_bytes_estimate());
                        self.core
                            .lock()
                            .make_read_room(publication_bytes.max(build_memory.bytes))
                            .map_err(|_| {
                                crate::exec::WorkRefusal::Session(Failure::ReadCapacity)
                            })?;
                        let resident = self
                            .core
                            .lock()
                            .station_read_retained_bytes()
                            .saturating_add(publication_bytes);
                        let resident_transition =
                            build_memory.prepare_resident(resident).map_err(|_| {
                                crate::exec::WorkRefusal::Session(Failure::ReadCapacity)
                            })?;
                        let mut durable_replica = self.core.replica_lock();
                        let receipt = prepared
                            .finalize_attached(&mut durable_replica, &commit, snapshot.as_ref())
                            .map_err(commit_failure)
                            .map_err(crate::exec::WorkRefusal::from)?;
                        replica = Some(durable_replica);
                        debug_assert_eq!(receipt, candidate_receipt);
                        resident_transition.commit();
                        Some((receipt, snapshot, publication))
                    }
                };
                let reply_snapshot = if let Some((receipt, snapshot, publication)) = fresh {
                    let committed_publication = publication.id;
                    let parent = prior.root();
                    let mut inner = self.core.lock();
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
                    inner.snapshot.clone()
                } else {
                    prior
                };
                {
                    let mut inner = self.core.lock();
                    consume_first_use_readies(
                        &mut inner,
                        std::slice::from_ref(&command),
                        &pinned_reader,
                        &ambient.world,
                    );
                }
                drop(replica);
                let reader =
                    SnapshotReader::interactive(reply_snapshot, self.core.body_images.clone());
                work_reply(&reader, &request)
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
        let reader =
            SnapshotReader::interactive(inner.snapshot.clone(), self.core.body_images.clone());
        drop(inner);
        let unresolved =
            crate::exec::scan_unresolved(&reader, &self.world_id).map_err(read_failure)?;
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
        let reader = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            SnapshotReader::interactive(inner.snapshot.clone(), self.core.body_images.clone())
        };
        // The Attempt's bounded Find client evaluates at the Run's parent
        // Manifest root, never at "latest": the interpretation the Grants
        // were instantiated against is the one the handler reads.
        let (run_state, _, _) = crate::exec::read_committed_run(&reader, &self.world_id, run)
            .map_err(read_failure)?
            .ok_or(Rejection::ContractViolation)?;
        let delegate = AttemptFindDelegate {
            session: self,
            pinned: crate::publication::PublicationId::new(
                run_state.started.parent_manifest_root,
                self.implementation,
                self.extractor_schema_digest,
            ),
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let dispatcher = crate::exec::Dispatcher::new(package, crate::exec::InProcess::new());
        let mut completion = dispatcher
            .invoke(
                &reader,
                &self.world_id,
                run,
                attempt,
                &cancel,
                Some(&delegate),
            )
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
        let (run_state, _, _) = crate::exec::read_committed_run(&reader, &self.world_id, run)
            .map_err(read_failure)?
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
        let (principal, ambient, pinned_reader, runtime, attempt) = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            let principal = self.fresh_principal().map_err(Failure::from)?;
            let ambient = self
                .ambient(&principal, inner.snapshot.root())
                .map_err(ambient_failure)?;
            let pinned_reader =
                SnapshotReader::interactive(inner.snapshot.clone(), self.core.body_images.clone());
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
                &pinned_reader,
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
            (principal, ambient, pinned_reader, runtime, attempt)
        };
        self.commit_runtime_effect(&principal, &ambient, operation, digest, label, runtime)?;
        {
            let mut inner = self.core.lock();
            consume_first_use_readies(
                &mut inner,
                std::slice::from_ref(&command),
                &pinned_reader,
                &ambient.world,
            );
        }
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
        let (principal, ambient, runtime) = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            let principal = self.fresh_principal().map_err(Failure::from)?;
            let ambient = self
                .ambient(&principal, inner.snapshot.root())
                .map_err(ambient_failure)?;
            let pinned_reader =
                SnapshotReader::interactive(inner.snapshot.clone(), self.core.body_images.clone());
            let runtime = lower_lifecycle_event(
                &pinned_reader,
                self.world.exec_specs(),
                &ambient,
                run,
                kind,
                content,
            )?;
            (principal, ambient, runtime)
        };
        self.commit_runtime_effect(&principal, &ambient, operation, digest, label, runtime)
    }

    fn commit_runtime_effect(
        &self,
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
        let _mutation = self.core.try_mutation_lane().map_err(commit_failure)?;
        let mut replica = self.core.replica_lock();
        let (prior_publication, prior, candidate_materialization, build_governor, build_station) = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            (
                inner.world_publications.get(&self.world_id).cloned(),
                inner.snapshot.clone(),
                inner.next_materialization,
                inner.read_memory.clone(),
                inner.station_memory.station,
            )
        };
        if StationCore::current_replica_root(&replica) != prior.root() {
            return Err(Failure::Conflict(Conflict::Body));
        }
        let parent_manifest_root = replica.manifest_root();
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
            parent_manifest_root,
            demand,
            intent_digest: digest,
            authorizer: &authorizer,
        };
        let outcome = replica
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
        let mut replica = Some(replica);
        let fresh = match outcome {
            replica::transaction::PreparedActionOutcome::Replayed(_) => None,
            replica::transaction::PreparedActionOutcome::Prepared(prepared) => {
                let candidate_receipt = prepared.receipt().map_err(commit_failure)?.clone();
                drop(replica.take());
                let snapshot_delta_bytes = prepared
                    .candidate_snapshot_delta_bytes_estimate(&prior)
                    .map_err(commit_failure)?;
                self.core
                    .lock()
                    .make_read_room(snapshot_delta_bytes)
                    .map_err(|_| Failure::ReadCapacity)?;
                let mut build_memory = build_governor
                    .reserve_build(build_station, snapshot_delta_bytes)
                    .map_err(|_| Failure::ReadCapacity)?;
                let snapshot = Arc::new(
                    prepared
                        .candidate_snapshot(&prior)
                        .map_err(commit_failure)?,
                );
                let snapshot_bytes = snapshot.retained_bytes_estimate();
                let corpus_memory = prior_publication
                    .as_ref()
                    .filter(|prior| {
                        prior.id.publication.implementation_digest == self.implementation
                            && prior.id.publication.extractor_schema_digest
                                == self.extractor_schema_digest
                    })
                    .map(|prior| {
                        prior.corpus.estimate_delta_build_bytes(
                            &snapshot,
                            &self.world_id,
                            self.world.find_extractors(),
                            &candidate_receipt.bodies,
                        )
                    })
                    .unwrap_or_else(|| {
                        crate::corpus::Corpus::estimate_build_bytes(
                            &snapshot,
                            &self.world_id,
                            self.world.find_extractors(),
                        )
                    });
                let corpus_build_bytes = corpus_memory
                    .retained_bytes
                    .saturating_add(corpus_memory.transient_bytes);
                self.core
                    .lock()
                    .make_read_room(snapshot_delta_bytes.saturating_add(corpus_build_bytes))
                    .map_err(|_| Failure::ReadCapacity)?;
                build_memory
                    .grow(corpus_build_bytes)
                    .map_err(|_| Failure::ReadCapacity)?;
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
                    self.core.body_images.clone(),
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
                    session_publication_failure(failure, "build candidate World publication")
                })?;
                let publication_bytes =
                    snapshot_bytes.saturating_add(publication.corpus.retained_bytes_estimate());
                self.core
                    .lock()
                    .make_read_room(publication_bytes.max(build_memory.bytes))
                    .map_err(|_| Failure::ReadCapacity)?;
                let resident = self
                    .core
                    .lock()
                    .station_read_retained_bytes()
                    .saturating_add(publication_bytes);
                let resident_transition = build_memory
                    .prepare_resident(resident)
                    .map_err(|_| Failure::ReadCapacity)?;
                let mut durable_replica = self.core.replica_lock();
                let receipt = prepared
                    .finalize_attached(&mut durable_replica, &commit, snapshot.as_ref())
                    .map_err(commit_failure)?;
                replica = Some(durable_replica);
                debug_assert_eq!(receipt, candidate_receipt);
                resident_transition.commit();
                Some((receipt, snapshot, publication))
            }
        };
        if let Some((receipt, snapshot, publication)) = fresh {
            let committed_publication = publication.id;
            let parent = prior.root();
            let mut inner = self.core.lock();
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
        drop(replica);
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
        self.find_selected(query, None)
    }

    /// Admit Find against one already-issued Station-local publication.
    ///
    /// Unlike [`Self::find`], this does not reconstruct or choose another
    /// materialization when the coordinate has expired. The portable
    /// `query.publication`, when present, must name the same semantic
    /// publication; Runtime then pins the exact retained Arc for evaluation.
    pub fn find_at(
        &self,
        publication: crate::publication::WorldPublicationId,
        query: crate::find::Query,
    ) -> Result<crate::find::Answer, crate::find::Failure> {
        self.find_selected(query, Some(publication))
    }

    #[allow(clippy::expect_used)]
    fn find_selected(
        &self,
        mut query: crate::find::Query,
        requested_world_publication: Option<crate::publication::WorldPublicationId>,
    ) -> Result<crate::find::Answer, crate::find::Failure> {
        struct BuildPlan {
            flight: Arc<PublicationFlight>,
            key: (WorldId, crate::publication::PublicationId),
            id: crate::publication::WorldPublicationId,
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
            build_memory: Option<BuildMemoryReservation>,
        }

        enum PublicationPlan {
            Ready(Arc<WorldPublication>),
            Follow(Arc<PublicationFlight>),
            Build(BuildPlan),
        }

        if !self.alive.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::find::Failure::Interrupted);
        }
        if let Some(exact) = requested_world_publication {
            if query
                .publication
                .is_some_and(|portable| portable != exact.publication)
            {
                return Err(
                    crate::find::Invalid::InvalidQuery("publication coordinate mismatch").into(),
                );
            }
            query.publication = Some(exact.publication);
        }
        let query_digest = query.digest()?;
        if !self.find_policy.bound.contains(query.bound) {
            return Err(crate::find::Failure::PolicyExceeded);
        }

        let requested_publication = query.publication;
        let cold_generation = if requested_world_publication.is_none() {
            requested_publication.and_then(|publication| {
                let inner = self.core.lock();
                (publication.manifest_root != inner.snapshot.root()
                    && !inner.generations.contains_key(&publication.manifest_root))
                .then(|| (publication.manifest_root, inner.snapshot.clone()))
            })
        } else {
            None
        };
        let cold_generation = if let Some((root, current)) = cold_generation {
            let reader = {
                let replica = self.core.replica_lock();
                replica.generation_reader(current)
            };
            let footprint = reader
                .generation_footprint(&root)
                .map_err(|_| crate::find::Failure::PublicationUnavailable)?
                .ok_or(crate::find::Failure::PublicationUnavailable)?;
            Some((root, reader, footprint))
        } else {
            None
        };
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
            let cursor_world_publication = cursor_coordinates
                .as_ref()
                .map(crate::find::Coordinates::world_publication);
            if let (Some(requested), Some(cursor)) =
                (requested_world_publication, cursor_world_publication)
            {
                if requested != cursor {
                    return Err(crate::find::Invalid::CursorMismatch("materialization").into());
                }
            }
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
                let publication = inner
                    .leased_world_publication(&key)
                    .ok_or(crate::find::Failure::PublicationExpired)?;
                PublicationPlan::Ready(publication)
            } else if let Some(id) = requested_world_publication {
                let key = (self.world_id.clone(), id);
                let publication = inner
                    .exact_world_publication(&key)
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
                        let root = semantic.manifest_root;
                        let snapshot = if root == inner.snapshot.root() {
                            Some((inner.snapshot.clone(), inner.snapshot_materialization))
                        } else {
                            inner.generations.get(&root).map(|generation| {
                                (generation.snapshot.clone(), generation.materialization)
                            })
                        };
                        let (reader, footprint) = if snapshot.is_some() {
                            (None, None)
                        } else {
                            let Some((cold_root, reader, footprint)) = cold_generation else {
                                return Err(crate::find::Failure::PublicationUnavailable);
                            };
                            if cold_root != root {
                                return Err(crate::find::Failure::PublicationUnavailable);
                            }
                            (Some(reader), Some(footprint))
                        };
                        let reserved_materialization = if let Some((_, materialization)) = snapshot
                        {
                            materialization
                        } else {
                            inner.reserve_materialization()
                        };
                        let id = crate::publication::WorldPublicationId::new(
                            semantic,
                            reserved_materialization,
                        );
                        let head = (self.world_id.clone(), id);
                        if let Some(WorldReadHead::Unavailable(failure)) =
                            inner.world_read_heads.get(&head).cloned()
                        {
                            return Err(find_publication_failure(failure));
                        }
                        let build_memory =
                            if let Some((estimate_snapshot, _)) = snapshot.as_ref() {
                                inner.reserve_full_publication_build(
                                    estimate_snapshot,
                                    &self.world_id,
                                    &find_extractors,
                                    true,
                                )
                            } else {
                                inner.reserve_historical_publication_build(
                                    footprint
                                        .as_ref()
                                        .ok_or(crate::find::Failure::PublicationUnavailable)?,
                                    &self.world_id,
                                    &find_extractors,
                                )
                            }
                            .map_err(|_| crate::find::Failure::CursorCapacityExceeded)?;
                        let flight = Arc::new(PublicationFlight::new());
                        inner
                            .publication_flights
                            .insert(key.clone(), flight.clone());
                        inner.world_read_heads.insert(head, WorldReadHead::Building);
                        let install_current = active_contract && root == inner.snapshot.root();
                        PublicationPlan::Build(BuildPlan {
                            flight,
                            key,
                            id,
                            root,
                            reserved_materialization,
                            snapshot,
                            reader,
                            world: find_world,
                            schemas: find_schemas,
                            extractors: find_extractors,
                            install_current,
                            build_memory: Some(build_memory),
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
            PublicationPlan::Build(mut plan) => {
                let build_memory = plan
                    .build_memory
                    .take()
                    .expect("new publication flight owns one build reservation");
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
                    let reconstructed = match plan
                        .reader
                        .as_ref()
                        .expect("cold build owns a generation reader")
                        .read_generation(&plan.root)
                    {
                        Ok(Some(snapshot)) => Arc::new(snapshot),
                        Ok(None) | Err(_) => {
                            record_world_read_failure(
                                &mut self.core.lock(),
                                (self.world_id.clone(), plan.id),
                                PublicationFailure::Generation,
                            );
                            let failure = crate::find::Failure::PublicationUnavailable;
                            let _ = finish(Err(failure.clone()));
                            return Err(failure);
                        }
                    };
                    let mut inner = self.core.lock();
                    if inner.closed {
                        record_world_read_failure(
                            &mut inner,
                            (self.world_id.clone(), plan.id),
                            PublicationFailure::Interrupted,
                        );
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
                if id != plan.id {
                    let mut inner = self.core.lock();
                    inner
                        .world_read_heads
                        .remove(&(self.world_id.clone(), plan.id));
                    inner
                        .world_read_heads
                        .insert((self.world_id.clone(), id), WorldReadHead::Building);
                }
                let corpus = match build_world_corpus(
                    &snapshot,
                    self.core.body_images.clone(),
                    &plan.world,
                    &self.world_id,
                    id,
                    &plan.schemas,
                    &plan.extractors,
                ) {
                    Ok(corpus) => Arc::new(corpus),
                    Err(cause) => {
                        let failure = find_publication_failure(cause.clone());
                        record_world_read_failure(
                            &mut self.core.lock(),
                            (self.world_id.clone(), id),
                            cause,
                        );
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
                        record_world_read_failure(
                            &mut inner,
                            (self.world_id.clone(), id),
                            PublicationFailure::Interrupted,
                        );
                        drop(inner);
                        let failure = crate::find::Failure::Interrupted;
                        let _ = finish(Err(failure.clone()));
                        return Err(failure);
                    }
                    if inner
                        .finish_publication_build(build_memory, &publication)
                        .is_err()
                    {
                        record_world_read_failure(
                            &mut inner,
                            (self.world_id.clone(), id),
                            PublicationFailure::Capacity,
                        );
                        drop(inner);
                        let failure = crate::find::Failure::CursorCapacityExceeded;
                        let _ = finish(Err(failure.clone()));
                        return Err(failure);
                    }
                    inner
                        .world_read_heads
                        .insert((self.world_id.clone(), id), WorldReadHead::Ready);
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

    #[cfg(test)]
    pub(crate) fn read_cache_stats_for_test(&self) -> (usize, u64, u64) {
        let inner = self.core.lock();
        (
            inner.generations.len(),
            inner.station_read_retained_bytes(),
            inner.retained_cache_bytes_limit,
        )
    }

    #[cfg(test)]
    pub(crate) fn constrain_read_cache_to_authoritative_headroom_for_test(&self) {
        let mut inner = self.core.lock();
        let mut snapshots = std::collections::BTreeSet::<usize>::new();
        let mut corpora = std::collections::BTreeSet::<usize>::new();
        let mut authoritative = 0u64;
        let mut current_headroom = 0u64;
        let mut add = |publication: &Arc<WorldPublication>| {
            let snapshot = Arc::as_ptr(&publication.snapshot) as usize;
            if snapshots.insert(snapshot) {
                authoritative =
                    authoritative.saturating_add(publication.snapshot.retained_bytes_estimate());
            }
            let corpus = Arc::as_ptr(&publication.corpus) as usize;
            if corpora.insert(corpus) {
                authoritative =
                    authoritative.saturating_add(publication.corpus.retained_bytes_estimate());
            }
        };
        for publication in inner.world_publications.values() {
            current_headroom = current_headroom.max(
                publication
                    .snapshot
                    .retained_bytes_estimate()
                    .saturating_add(publication.corpus.retained_bytes_estimate()),
            );
            add(publication);
        }
        for lease in inner.cursor_leases.values() {
            add(&lease.publication);
        }
        for publication in inner.publication_retention.publications() {
            add(&publication);
        }
        drop(add);
        inner.retained_cache_bytes_limit = authoritative
            .saturating_add(current_headroom)
            .saturating_add(16 * 1024 * 1024)
            .min(inner.read_memory.station_bytes);
    }

    #[cfg(test)]
    pub(crate) fn constrain_read_cache_to_resident_only_for_test(&self) {
        let mut inner = self.core.lock();
        inner.retained_cache_bytes_limit = inner.station_read_retained_bytes();
    }

    /// Query one exact World publication through the same resolution path used
    /// by Find. `query.publication == None` selects the authority-active current
    /// package and root. An explicit PublicationId resolves its exact installed
    /// implementation/extractor contract; Runtime never interprets an old root
    /// through ambient code. Cold generation reconstruction and extraction run
    /// outside the Station writer and are single-flighted with Find.
    pub fn query(&self, query: Query) -> Result<Projection, Failure> {
        self.query_selected(query, None)
    }

    /// Query one already-issued Station-local World publication exactly.
    ///
    /// The full coordinate is deliberately separate from portable World query
    /// intent: materialization ids are local to this Station activation and
    /// must not be persisted as semantic application state. An expired exact
    /// image is reported as such; Runtime never substitutes the current image
    /// or another materialization with the same Manifest root.
    pub fn query_at(
        &self,
        publication: crate::publication::WorldPublicationId,
        query: Query,
    ) -> Result<Projection, Failure> {
        self.query_selected(query, Some(publication))
    }

    #[allow(clippy::expect_used)]
    fn query_selected(
        &self,
        mut query: Query,
        requested_world_publication: Option<crate::publication::WorldPublicationId>,
    ) -> Result<Projection, Failure> {
        struct BuildPlan {
            flight: Arc<PublicationFlight>,
            key: (WorldId, crate::publication::PublicationId),
            id: crate::publication::WorldPublicationId,
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
            build_memory: Option<BuildMemoryReservation>,
        }

        enum Plan {
            Ready(Arc<WorldPublication>),
            Follow {
                flight: Arc<PublicationFlight>,
                id: crate::publication::WorldPublicationId,
            },
            Build(BuildPlan),
        }

        self.ensure_live()?;
        if let Some(exact) = requested_world_publication {
            if query
                .publication
                .is_some_and(|portable| portable != exact.publication)
            {
                return Err(Rejection::ContractViolation.into());
            }
            query.publication = Some(exact.publication);
        }
        self.ensure_within_limit(query.payload.len())?;
        let requested = query.publication;
        let principal = self.fresh_principal()?;
        let cold_generation = if requested_world_publication.is_none() {
            requested.and_then(|publication| {
                let inner = self.core.lock();
                (publication.manifest_root != inner.snapshot.root()
                    && !inner.generations.contains_key(&publication.manifest_root))
                .then(|| (publication.manifest_root, inner.snapshot.clone()))
            })
        } else {
            None
        };
        let cold_generation = if let Some((root, current)) = cold_generation {
            let reader = {
                let replica = self.core.replica_lock();
                replica.generation_reader(current)
            };
            let footprint = reader
                .generation_footprint(&root)
                .map_err(|_| Failure::GenerationUnavailable)?
                .ok_or(Failure::GenerationUnavailable)?;
            Some((root, reader, footprint))
        } else {
            None
        };
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

            let plan = if let Some(id) = requested_world_publication {
                let key = (self.world_id.clone(), id);
                let publication = inner
                    .exact_world_publication(&key)
                    .ok_or(Failure::PublicationExpired(id))?;
                Plan::Ready(publication)
            } else if let Some(publication) = inner
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
                Plan::Ready(publication)
            } else {
                let key = (self.world_id.clone(), semantic);
                if let Some(flight) = inner.publication_flights.get(&key).cloned() {
                    let Some(id) = inner
                        .world_read_heads
                        .iter()
                        .find_map(|((world, id), head)| {
                            (world == &self.world_id
                                && id.publication == semantic
                                && matches!(head, WorldReadHead::Building))
                            .then_some(*id)
                        })
                    else {
                        return Err(Failure::Interrupted);
                    };
                    Plan::Follow { flight, id }
                } else {
                    let root = semantic.manifest_root;
                    let snapshot = if root == inner.snapshot.root() {
                        Some((inner.snapshot.clone(), inner.snapshot_materialization))
                    } else {
                        inner.generations.get(&root).map(|generation| {
                            (generation.snapshot.clone(), generation.materialization)
                        })
                    };
                    let (reader, footprint) = if snapshot.is_some() {
                        (None, None)
                    } else {
                        let Some((cold_root, reader, footprint)) = cold_generation else {
                            return Err(Failure::GenerationUnavailable);
                        };
                        if cold_root != root {
                            return Err(Failure::GenerationUnavailable);
                        }
                        (Some(reader), Some(footprint))
                    };
                    let reserved_materialization = if let Some((_, materialization)) = snapshot {
                        materialization
                    } else {
                        inner.reserve_materialization()
                    };
                    let id = crate::publication::WorldPublicationId::new(
                        semantic,
                        reserved_materialization,
                    );
                    let head = (self.world_id.clone(), id);
                    if let Some(WorldReadHead::Unavailable(failure)) =
                        inner.world_read_heads.get(&head).cloned()
                    {
                        return Err(session_publication_failure(
                            failure,
                            "resolve cached exact World query publication",
                        ));
                    }
                    // See the Find path above: never price a cold historical
                    // generation from an unrelated ambient snapshot. Its
                    // authenticated footprint includes retained and complete
                    // reconstruction-transient bounds before any delta I/O.
                    let build_memory = if let Some((estimate_snapshot, _)) = snapshot.as_ref() {
                        inner.reserve_full_publication_build(
                            estimate_snapshot,
                            &self.world_id,
                            &find_extractors,
                            true,
                        )
                    } else {
                        inner.reserve_historical_publication_build(
                            footprint.as_ref().ok_or(Failure::GenerationUnavailable)?,
                            &self.world_id,
                            &find_extractors,
                        )
                    }
                    .map_err(|_| Failure::ReadCapacity)?;
                    let flight = Arc::new(PublicationFlight::new());
                    inner
                        .publication_flights
                        .insert(key.clone(), flight.clone());
                    inner.world_read_heads.insert(head, WorldReadHead::Building);
                    Plan::Build(BuildPlan {
                        flight,
                        key,
                        id,
                        root,
                        reserved_materialization,
                        snapshot,
                        reader,
                        world: query_world.clone(),
                        find_schemas: find_schemas.clone(),
                        find_extractors: find_extractors.clone(),
                        install_current: active_contract && root == inner.snapshot.root(),
                        build_memory: Some(build_memory),
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
            Plan::Follow { flight, id } => match flight.wait() {
                Ok(publication) => publication,
                Err(failure) => {
                    let exact = self
                        .core
                        .lock()
                        .world_read_heads
                        .get(&(self.world_id.clone(), id))
                        .cloned();
                    if let Some(WorldReadHead::Unavailable(cause)) = exact {
                        return Err(session_publication_failure(
                            cause,
                            "follow exact World query publication build",
                        ));
                    }
                    return Err(query_publication_failure(failure));
                }
            },
            Plan::Build(mut plan) => {
                let build_memory = plan
                    .build_memory
                    .take()
                    .expect("new query publication flight owns one build reservation");
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
                    let reconstructed = match plan
                        .reader
                        .as_ref()
                        .expect("cold query build owns generation reader")
                        .read_generation(&plan.root)
                    {
                        Ok(Some(snapshot)) => Arc::new(snapshot),
                        Ok(None) | Err(_) => {
                            record_world_read_failure(
                                &mut self.core.lock(),
                                (self.world_id.clone(), plan.id),
                                PublicationFailure::Generation,
                            );
                            let failure = crate::find::Failure::PublicationUnavailable;
                            let _ = finish(Err(failure));
                            return Err(Failure::GenerationUnavailable);
                        }
                    };
                    let mut inner = self.core.lock();
                    if inner.closed {
                        record_world_read_failure(
                            &mut inner,
                            (self.world_id.clone(), plan.id),
                            PublicationFailure::Interrupted,
                        );
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
                if id != plan.id {
                    let mut inner = self.core.lock();
                    inner
                        .world_read_heads
                        .remove(&(self.world_id.clone(), plan.id));
                    inner
                        .world_read_heads
                        .insert((self.world_id.clone(), id), WorldReadHead::Building);
                }
                let corpus = match build_world_corpus(
                    &snapshot,
                    self.core.body_images.clone(),
                    &plan.world,
                    &self.world_id,
                    id,
                    &plan.find_schemas,
                    &plan.find_extractors,
                ) {
                    Ok(corpus) => Arc::new(corpus),
                    Err(cause) => {
                        let session_failure = session_publication_failure(
                            cause.clone(),
                            "prepare exact World query publication",
                        );
                        let find_failure = find_publication_failure(cause.clone());
                        record_world_read_failure(
                            &mut self.core.lock(),
                            (self.world_id.clone(), id),
                            cause,
                        );
                        let _ = finish(Err(find_failure));
                        return Err(session_failure);
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
                        record_world_read_failure(
                            &mut inner,
                            (self.world_id.clone(), id),
                            PublicationFailure::Interrupted,
                        );
                        drop(inner);
                        let failure = crate::find::Failure::Interrupted;
                        let _ = finish(Err(failure));
                        return Err(Failure::Interrupted);
                    }
                    if inner
                        .finish_publication_build(build_memory, &publication)
                        .is_err()
                    {
                        record_world_read_failure(
                            &mut inner,
                            (self.world_id.clone(), id),
                            PublicationFailure::Capacity,
                        );
                        drop(inner);
                        let failure = crate::find::Failure::CursorCapacityExceeded;
                        let _ = finish(Err(failure));
                        return Err(Failure::ReadCapacity);
                    }
                    inner
                        .world_read_heads
                        .insert((self.world_id.clone(), id), WorldReadHead::Ready);
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
        let replica = self.core.replica_lock();
        let durable = replica
            .read_generations()
            .map_err(|_| Failure::Persistence)?;
        drop(replica);
        let inner = self.core.lock();
        if inner.closed {
            return Err(Failure::Interrupted);
        }
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
        let reader = SnapshotReader::interactive(
            publication.snapshot.clone(),
            self.core.body_images.clone(),
        );
        let (read_memory, station_memory, publication_retention, admitted_retained_bytes) = {
            let inner = self.core.lock();
            if inner.closed {
                return Err(Failure::Interrupted);
            }
            (
                inner.read_memory.clone(),
                inner.station_memory.clone(),
                inner.publication_retention.clone(),
                inner
                    .station_read_retained_bytes()
                    .saturating_add(inner.publication_incremental_bytes(&publication)),
            )
        };
        let issued_find_cursor = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let find = crate::world::FindHandle::new(Arc::new(ContextFindReader {
            read_memory,
            station_memory,
            publication_retention,
            admitted_retained_bytes,
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
            inner.snapshot.frontier()
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

    #[test]
    fn exact_read_heads_do_not_terminally_cache_retryable_body_failures() {
        let world = WorldId::parse("com.example.retryable-read").unwrap();
        let coordinate = crate::world::BodyReadCoordinate::new(key(&world, 0x31), Some([0x32; 32]));
        let extractor = |failure| PublicationFailure::Extractor {
            source: None,
            body: None,
            stage: "callback-rejection",
            rejection: Some(Rejection::BodyRead(failure)),
        };

        let capacity = extractor(crate::world::BodyReadFailure::Capacity(coordinate.clone()));
        assert!(capacity.is_retryable());
        assert_eq!(
            find_publication_failure(capacity),
            crate::find::Failure::CursorCapacityExceeded
        );

        let interrupted = extractor(crate::world::BodyReadFailure::Interrupted(
            coordinate.clone(),
        ));
        assert!(interrupted.is_retryable());
        assert_eq!(
            find_publication_failure(interrupted),
            crate::find::Failure::Interrupted
        );

        let unavailable = extractor(crate::world::BodyReadFailure::KeyUnavailable(coordinate));
        assert!(!unavailable.is_retryable());
        assert_eq!(
            find_publication_failure(unavailable),
            crate::find::Failure::Unavailable
        );
    }

    #[test]
    fn indeterminate_durability_is_never_collapsed_into_retryable_persistence() {
        assert_eq!(
            commit_failure(replica::transaction::commit::Failure::OutcomeUnknown),
            Failure::OutcomeUnknown
        );
    }

    #[test]
    fn occupied_mutation_lane_is_a_typed_non_admission() {
        assert_eq!(
            commit_failure(replica::transaction::commit::Failure::MutationBusy),
            Failure::Busy
        );
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
        let active = crate::exec::active_run_body_key(&world, run);
        let mut operations = vec![
            (key.clone(), Op::Create),
            (
                active.clone(),
                Op::ReplaceAtomic {
                    value: run.as_bytes().to_vec(),
                },
            ),
        ];
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
            SchemaId::parse(crate::exec::ACTIVE_RUN_BODY_SCHEMA).unwrap(),
            crate::exec::ACTIVE_RUN_BODY_SCHEMA_VERSION,
            EncodingId::parse(crate::exec::BODY_ENCODING).unwrap(),
            replica::body::MUTATION_ATOMIC,
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
                    actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
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
                vec![key.clone(), active.clone()],
                "seed-returned",
                &operations,
                &[
                    (key, run_binding().unwrap()),
                    (active, active_run_binding().unwrap()),
                ],
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
    fn immutable_atomic_keeps_its_protocol_tag_and_one_shot_operation_family() {
        let immutable = MutationModel::ImmutableAtomic;
        assert_eq!(
            mutation_model_tag(&immutable),
            replica::body::MUTATION_IMMUTABLE_ATOMIC
        );
        assert!(operation_matches_mutation_model(
            &immutable,
            &Op::ReplaceAtomic { value: vec![1] },
        ));
        assert!(!operation_matches_mutation_model(
            &immutable,
            &Op::Tombstone,
        ));
        assert!(!operation_matches_mutation_model(&immutable, &Op::Create));
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
            .expect("the protected Run Body is readable")
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
        assert_eq!(lowered.operations.len(), 2);
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
        assert_eq!(
            lowered.operations[1],
            (
                crate::exec::active_run_body_key(&ambient.world, run),
                Op::Tombstone,
            ),
            "Run acceptance atomically removes the sparse active marker",
        );
        assert!(lowered.bindings.iter().any(|(key, binding)| {
            key == &crate::exec::active_run_body_key(&ambient.world, run)
                && binding.schema.as_str() == crate::exec::ACTIVE_RUN_BODY_SCHEMA
                && binding.mutation_model == replica::body::MUTATION_ATOMIC
        }));

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
                    actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
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
