#![allow(
    clippy::arithmetic_side_effects,
    reason = "lifecycle generations and bounded task counts are monotonic within process limits"
)]
//! The orbital lifecycle handles: [`Runtime`], [`Orbit`], and [`Station`].
//!
//! An Orbit is the durable relationship and persists while vacant or occupied.
//! [`Orbit::activate`] consumes its vacant handle and returns a [`Station`];
//! [`Station::vacate`] consumes the active handle and returns a vacant Orbit
//! handle. The consuming API expresses exclusive operational ownership, not a
//! conversion of a Station into an Orbit. Runtime is cloneable and owns
//! configuration + registrations; it owns no active Space state. Orbit and
//! Station are **not** cloneable.
//!
//! The durable footprint is real: an Orbit is backed by an on-disk store
//! ([`crate::store`]) and holds the exclusive store lock (operational
//! ownership). Activation durably increments the store epoch, opens the
//! journaled Replica (running crash recovery), and moves the lock into the
//! Station, which owns a cancellation token and a tracked task set. Dormancy
//! drains those tasks in a fixed order and releases the lock **last**.
//! [`Station::neighbors`] and [`Station::contact`] are incomplete surfaces
//! until completion package C2 (`docs/plans/02-runtime-world-carve.md`)
//! delivers the persistent Neighbor registry and Contact orchestration.

use crate::poison::LockRecovering;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
// `tokio::time::Instant`, not `tokio::time::Instant`. Without the `test-util`
// feature it IS `tokio::time::Instant::now()` — same call, same value, no
// indirection — so production pays nothing. With it, `tokio::time::pause()`
// stops the clock for every site at once, which is what lets a test drive a
// sweep interval or a probation window without waiting for one.
use std::time::Duration;
use tokio::time::Instant;

use mechanics::{
    ids::SpaceId,
    station::{Epoch, Key},
};

use crate::registry::{Builder, Catalog};
use crate::session::Session;
use crate::store::{OrbitStore, StoreLock};
use crate::world::{AuthorityView, LocalIdentity, PrincipalFacts};
use replica::body::BodyKeySource;
use replica::body::WorldId;
use replica::convergence::ConvergenceOutcome;

/// The authority view a Runtime without one falls back to: nobody resolves, so
/// nothing can dock. Membership exists only when the deployment supplies a real
/// mechanics view.
struct DenyAllAuthority;

impl AuthorityView for DenyAllAuthority {
    fn resolve(
        &self,
        _device: &mechanics::ids::DeviceId,
    ) -> Option<crate::world::PrincipalResolution> {
        None
    }
}

/// The key source a Runtime without one falls back to: no sealing or opening
/// material, so durable local writes fail closed with `BodyKeyUnavailable`
/// and all protected remote material stays opaque.
struct NoBodyKeys;

impl BodyKeySource for NoBodyKeys {
    fn sealing_key(&self) -> Option<mechanics::authorization::AuthorizedBodyKey> {
        None
    }
    fn opening_key(
        &self,
        _epoch: &[u8; 16],
    ) -> Option<mechanics::authorization::AuthorizedBodyKey> {
        None
    }
}

/// The default deadline for draining tracked tasks during dormancy.
pub const DEFAULT_DRAIN_DEADLINE: Duration = Duration::from_secs(10);

/// A failure while creating, finding, opening, inspecting, or removing an
/// Orbit. The operation's receiver supplies the missing context; this type is
/// deliberately owned by the lifecycle namespace instead of a crate-wide
/// error bag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    OrbitNotFound(SpaceId),
    ReplicaLocked(SpaceId),
    UnsupportedCoordinatesVersion,
    Integrity(Integrity),
    NoStoreRoot,
    AlreadyExists(SpaceId),
    EpochOverflow,
    Persistence(Persistence),
    StationDormant,
    PrincipalDenied,
    NoActiveImplementation(WorldId),
    ImplementationUnavailable {
        world: WorldId,
        implementation: [u8; 32],
    },
    AuthorityUnavailable(String),
    /// The activation's Find policy failed validation. Carries the refusal:
    /// `Policy` has one field today, but `find::Invalid` names fifteen ways a
    /// contract can be refused, and a discard here is how a diagnosable
    /// refusal becomes one word the moment the policy grows a second field —
    /// the exact defect the error-context ratchet exists to stop.
    InvalidFindPolicy(crate::find::Invalid),
    /// A hosted World's extractor declaration could not be reduced to the
    /// canonical digest that binds publications and cursors.
    InvalidExtractorSchema(crate::find::Invalid),
    WorldPublicationUnavailable(WorldId),
    /// The composition-wide read-memory governor refused another Station or
    /// publication build before allocating beyond its physical envelope.
    ReadCapacity,
    UnknownWorld(WorldId),
}

/// The durable invariant that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    Coordinates,
    Marker,
    SpaceIdentity,
    Epoch,
    Replica,
    NeighborRegistry,
    StationKey,
    RemovalConfirmation,
}

/// The local resource that prevented a lifecycle operation from completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    Entropy,
    Io(std::io::ErrorKind),
    Replica,
    Cache,
    InjectedFault,
}

/// A failure while a Station drains and returns its Orbit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interruption {
    DrainTimeout,
    Checkpoint,
    Transport,
}

/// Why an activation ended without an orderly vacate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    TaskFailed,
    Vacate(Interruption),
}

/// The stopped Station's recoverable Orbit and any abnormal exit reason.
#[derive(Debug)]
pub struct Exit {
    pub orbit: Orbit,
    pub reason: Option<ExitReason>,
}

macro_rules! lifecycle_error {
    ($($ty:ty),+ $(,)?) => {$(
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{self:?}")
            }
        }
        impl std::error::Error for $ty {}
    )+ };
}

lifecycle_error!(Failure, Interruption, ExitReason);

/// Where the resident cache lives, under the Orbit store directory.
///
/// A sibling of the journal rather than inside it. The journal promises that
/// everything a root names is present and intact; a content chunk is optional
/// by design, and losing one should mean "fetch it again" rather than "this
/// store is broken".
const CACHE_DIR: &str = "content-cache";

/// A cooperative cancellation signal shared by a Station and its tracked tasks.
/// A task polls [`CancelToken::is_cancelled`] and exits promptly when set. The
/// API cannot preempt a task that ignores it — such a task is drained on a
/// deadline and, if it will not stop, leaked (never holding the store lock).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    /// Request cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// How the local Exec drain paces its own labor.
///
/// Options rather than constants, per the compatibility doctrine: a bound an
/// operator cannot set is decorative. Every field paces the Station's *own*
/// work — none of this ranks candidates for a Run, which stays behind the
/// direction gate. Hand-written `Default` because zero fails closed: a
/// zero-action pass performs nothing, forever.
///
/// There is deliberately no failure backoff here yet. Under the current
/// failure classes only an Unknown-classed stale lease is ever re-tried,
/// and delaying that would delay honest crash recovery; a retry loop a
/// backoff could tame only exists once an external performer backend can
/// die mid-Attempt. The field joins this struct in the same commit as the
/// backend that makes it real — never before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecPacing {
    /// Most perform actions one drain pass may commit.
    pub actions_per_pass: u32,
    /// The idle beat between drain passes; a fresh commit still wakes the
    /// drain immediately through the exec tick.
    pub drain_interval: Duration,
    /// How long a resolved, uncited Run is kept before its reserved Body is
    /// tombstoned. `None` — the default — keeps everything forever: disposal
    /// of signed history is something an operator turns on, never something
    /// a default does. Eligibility is a reachability projection; this window
    /// is only the grace on top of it, counted in activation memory, so a
    /// restart forgets elapsed grace and can only ever delay disposal.
    pub retention_window: Option<Duration>,
}

impl Default for ExecPacing {
    fn default() -> Self {
        Self {
            actions_per_pass: 16,
            drain_interval: Duration::from_millis(50),
            retention_window: None,
        }
    }
}

/// Options for activating an Orbit into a Station.
#[derive(Debug, Default)]
pub struct Activation {
    /// The deadline for draining tracked tasks at dormancy.
    pub drain_deadline: Duration,
    /// How the local Exec drain paces its own labor.
    pub exec: ExecPacing,
    /// The content plane's local policy: how much disk it may hold, and how
    /// large a single content this Station will accept.
    pub content: ContentOptions,
    /// Local ceilings for the Find evaluator.
    pub find: crate::find::Policy,
    /// Which delivery planes this Station answers on.
    pub planes: PlaneOptions,
    /// The Station's Contact plane: transport, station identity, mechanics
    /// seams, and gossip. `None` activates an offline Station (valid; grants
    /// no new authority; `neighbors` still serves the persisted registry).
    pub comms: Option<crate::contact_driver::CommsOptions>,
    /// The Observation ring capacity (`0` = the default 1024; hard maximum
    /// 65,536).
    pub observation_capacity: usize,
}

impl Activation {
    /// The default activation: offline, with the default drain deadline.
    pub fn offline() -> Self {
        Self {
            drain_deadline: DEFAULT_DRAIN_DEADLINE,
            exec: ExecPacing::default(),
            content: ContentOptions::default(),
            find: crate::find::Policy::default(),
            planes: PlaneOptions::default(),
            comms: None,
            observation_capacity: 0,
        }
    }
}

/// Local policy for the content plane.
///
/// Hand-written `Default` rather than derived, because both fields fail closed
/// at zero: a zero quota would sweep every chunk the moment it landed, and a
/// zero maximum would refuse every content. A caller that leaves this alone
/// should get a working Station, not a silently disabled one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentOptions {
    /// Resident bytes this Station will hold before a sweep starts evicting.
    ///
    /// A target, not a guarantee: every chunk of a committed content is held by
    /// that content's own lease, so a cache full of committed content has no
    /// eligible victims. The sweep reports the shortfall rather than hiding it.
    pub cache_quota_bytes: u64,
    /// The largest single content this Station will ingest or fetch. May only
    /// *lower* the protocol maximum, never raise it.
    pub max_content_len: u64,
}

impl Default for ContentOptions {
    fn default() -> Self {
        Self {
            cache_quota_bytes: 4 * 1024 * 1024 * 1024,
            max_content_len: 256 * 1024 * 1024,
        }
    }
}

/// Local policy for the delivery planes.
///
/// Hand-written `Default` for the same reason [`ContentOptions`] has one: a
/// derived `Default` would leave every plane off, and a caller that says
/// nothing should get a Station that works rather than one that is silently
/// deaf. Turning a plane off is a thing an operator does on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneOptions {
    /// Whether this Station serves and fetches content over Freight.
    pub freight_enabled: bool,
    /// Whether this Station answers on the Live plane.
    pub live_enabled: bool,
    /// Whether this Station answers on the Exec plane.
    pub exec_enabled: bool,
    /// Whether a file offered by one of this identity's own devices may land on
    /// disk without anyone clicking.
    ///
    /// The one option here that defaults to *off*, and deliberately breaking
    /// this struct's own fail-open rule. Every other field enables something a
    /// peer asked for; this one writes to a disk nobody asked about at the
    /// moment it happens, and an operator who has not said yes has not said yes.
    pub auto_accept_offers: bool,
}

impl Default for PlaneOptions {
    fn default() -> Self {
        Self {
            freight_enabled: true,
            live_enabled: true,
            exec_enabled: true,
            auto_accept_offers: false,
        }
    }
}

impl PlaneOptions {
    fn policy(self) -> crate::admission::PlanePolicy {
        crate::admission::PlanePolicy {
            serve_enabled: self.freight_enabled,
            fetch_enabled: self.freight_enabled,
            live_enabled: self.live_enabled,
            exec_enabled: self.exec_enabled,
            auto_accept_offers: self.auto_accept_offers,
        }
    }
}

/// An explicit, non-defaultable confirmation that destructive removal is
/// intended. Constructing it names the exact Space being removed, so a stray
/// call cannot destroy the wrong Orbit.
#[derive(Debug, Clone)]
pub struct RemovalConfirmation {
    space: SpaceId,
}

impl RemovalConfirmation {
    /// Confirm destructive removal of a specific Space's local Orbit.
    pub fn for_space(space: SpaceId) -> Self {
        Self { space }
    }
    pub fn space(&self) -> &SpaceId {
        &self.space
    }
}

/// The cloneable entry point. Owns configuration (the store root) and the
/// immutable World registry; owns no active Space state. Local Orbit discovery
/// is Runtime's; acquisition/activation live on the returned [`Orbit`]/
/// [`Station`].
#[derive(Clone)]
pub struct Runtime {
    registry: Catalog,
    root: Option<PathBuf>,
    /// The mechanics authority view principals are derived from. Sessions and
    /// Worlds cannot replace it; only the composition root supplies it.
    authority: Arc<dyn AuthorityView>,
    /// The mechanics-owned Body key source: seals local commits and opens
    /// protected material. Supplied by the composition root; absent keys fail
    /// closed (local writes refuse, remote material stays opaque).
    keys: Arc<dyn BodyKeySource>,
    read_memory: Arc<crate::session::ReadMemoryGovernor>,
}

impl Runtime {
    /// Begin building a Runtime by registering Worlds.
    pub fn builder() -> Builder {
        Builder::new()
    }

    /// Wrap a frozen registry into a Runtime with **no** store root and a
    /// deny-all authority. Such a Runtime can host Worlds but cannot form or
    /// acquire a durable Orbit, and nothing can dock.
    pub fn from_registry(registry: Catalog) -> Self {
        Self {
            registry,
            root: None,
            authority: Arc::new(DenyAllAuthority),
            keys: Arc::new(NoBodyKeys),
            read_memory: crate::session::ReadMemoryGovernor::process_default(),
        }
    }

    /// Open a Runtime rooted at a store directory, with the mechanics authority
    /// view principals are derived from and the mechanics-owned Body key
    /// source that seals/opens protected material. Orbits live under
    /// `<root>/<space-id>/`.
    pub fn open(
        root: impl Into<PathBuf>,
        registry: Catalog,
        authority: Arc<dyn AuthorityView>,
        keys: Arc<dyn BodyKeySource>,
    ) -> Self {
        Self {
            registry,
            root: Some(root.into()),
            authority,
            keys,
            read_memory: crate::session::ReadMemoryGovernor::process_default(),
        }
    }

    /// Authenticate a local caller by possession of its device seed. The device
    /// key is **derived** from the seed — an identity cannot be asserted from a
    /// bare device id, and standing is resolved by the [`AuthorityView`] at dock
    /// and again at every submit, never carried by the identity.
    pub fn identity_from_seed(seed: &[u8; 32]) -> LocalIdentity {
        LocalIdentity::from_seed(seed)
    }

    /// The immutable World registry this Runtime hosts.
    pub fn registry(&self) -> &Catalog {
        &self.registry
    }

    fn root(&self) -> Result<&PathBuf, Failure> {
        self.root.as_ref().ok_or(Failure::NoStoreRoot)
    }

    /// Form a new Space and its founding Orbit: mint a fresh SpaceId, create the
    /// store (marker + zero epoch), and acquire the exclusive lock. The full
    /// mechanics founding proof and Coordinates minting arrive with the product
    /// cutover (completion package C5); the durable Orbit and its lock are real
    /// here.
    pub fn create(&self) -> Result<Orbit, Failure> {
        let root = self.root()?;
        let mut digest = [0u8; 16];
        getrandom::fill(&mut digest).map_err(|_| Failure::Persistence(Persistence::Entropy))?;
        let space = SpaceId::from_digest(digest);
        let store = OrbitStore::create(root, &space)?;
        let lock = store.acquire_lock()?;
        let epoch = Epoch::from_u64(store.read_epoch()?);
        Ok(Orbit::new(
            store,
            self.registry.clone(),
            self.authority.clone(),
            self.keys.clone(),
            self.read_memory.clone(),
            epoch,
            lock,
        ))
    }

    /// Materialize this device's Orbit from Coordinates. The Coordinates are
    /// fully verified (version, founding self-proof, approach-Station signature,
    /// admission structure); pre-carve invitation bytes fail with
    /// [`Failure::UnsupportedCoordinatesVersion`]. The store is created if
    /// absent and locked. Admission redemption and initial authority/Replica
    /// import arrive with the product cutover (completion package C5).
    pub fn materialize(
        &self,
        coordinates: &crate::coordinates::SignedCoordinates,
    ) -> Result<Orbit, Failure> {
        let root = self.root()?;
        let verified = coordinates.verify().map_err(|e| match e {
            crate::coordinates::Invalid::UnsupportedVersion(_) => {
                Failure::UnsupportedCoordinatesVersion
            }
            _ => Failure::Integrity(Integrity::Coordinates),
        })?;
        let store = match OrbitStore::open(root, &verified.space) {
            Ok(store) => store,
            Err(Failure::OrbitNotFound(_)) => OrbitStore::create(root, &verified.space)?,
            Err(e) => return Err(e),
        };
        let lock = store.acquire_lock()?;
        let epoch = Epoch::from_u64(store.read_epoch()?);
        Ok(Orbit::new(
            store,
            self.registry.clone(),
            self.authority.clone(),
            self.keys.clone(),
            self.read_memory.clone(),
            epoch,
            lock,
        ))
    }

    /// Acquire an existing local Orbit for operational ownership. Revalidates the
    /// store marker/version and takes the exclusive lock (a second acquisition
    /// while a live Station holds it fails with
    /// [`Failure::ReplicaLocked`]).
    pub fn acquire(&self, space: &SpaceId) -> Result<Orbit, Failure> {
        let root = self.root()?;
        let store = OrbitStore::open(root, space)?;
        let lock = store.acquire_lock()?;
        let epoch = Epoch::from_u64(store.read_epoch()?);
        Ok(Orbit::new(
            store,
            self.registry.clone(),
            self.authority.clone(),
            self.keys.clone(),
            self.read_memory.clone(),
            epoch,
            lock,
        ))
    }

    /// Advisory, read-only observation of a local Orbit. Never takes the lock and
    /// never grants control.
    pub fn inspect(&self, space: &SpaceId) -> Result<OrbitStatus, Failure> {
        let root = self.root()?;
        let store = OrbitStore::open(root, space)?;
        Ok(OrbitStatus {
            space: space.clone(),
            locked: store.is_locked(),
        })
    }

    /// Advisory observation of every discoverable local Orbit.
    pub fn list(&self) -> Vec<OrbitStatus> {
        let Ok(root) = self.root() else {
            return Vec::new();
        };
        OrbitStore::list(root)
            .into_iter()
            .filter_map(|space| self.inspect(&space).ok())
            .collect()
    }
}

/// One device's durable relationship to a Space, acquired for operational
/// ownership (it holds the store lock). **Not** cloneable: [`Orbit::activate`]
/// consumes it.
pub struct Orbit {
    store: OrbitStore,
    registry: Catalog,
    authority: Arc<dyn AuthorityView>,
    keys: Arc<dyn BodyKeySource>,
    read_memory: Arc<crate::session::ReadMemoryGovernor>,
    epoch: Epoch,
    lock: StoreLock,
}

impl std::fmt::Debug for Orbit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orbit")
            .field("space", self.store.space())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl Orbit {
    pub(crate) fn new(
        store: OrbitStore,
        registry: Catalog,
        authority: Arc<dyn AuthorityView>,
        keys: Arc<dyn BodyKeySource>,
        read_memory: Arc<crate::session::ReadMemoryGovernor>,
        epoch: Epoch,
        lock: StoreLock,
    ) -> Self {
        Self {
            store,
            registry,
            authority,
            keys,
            read_memory,
            epoch,
            lock,
        }
    }

    /// The Space this Orbit relates to.
    pub fn space_id(&self) -> &SpaceId {
        self.store.space()
    }

    /// The current durable activation epoch.
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// Occupy this Orbit with a [`Station`], consuming the vacant handle.
    /// Activation first durably increments and fsyncs the store epoch (failing
    /// closed on overflow), then transfers the store lock into the live Station.
    /// The durable Orbit remains the same participation. Valid offline; grants
    /// no new Space authority.
    fn open_station(self, options: Activation) -> Result<Station, Failure> {
        options
            .find
            .validate()
            .map_err(Failure::InvalidFindPolicy)?;
        let drain_deadline = if options.drain_deadline.is_zero() {
            DEFAULT_DRAIN_DEADLINE
        } else {
            options.drain_deadline
        };
        let epoch = Epoch::from_u64(self.store.bump_epoch()?);
        // Open the durable Replica at the Orbit store's journaled Engine store:
        // crash recovery runs here (exposing the complete old or complete new
        // state), and from then on every acknowledged commit has completed the
        // full journal protocol before `submit` returns. A crash, kill, or
        // `wait` exit after an acknowledged commit loses nothing.
        let mut replica = replica::Replica::open(self.store.replica_dir()?, self.keys.clone())
            .map_err(|e| match e {
                replica::transaction::commit::Failure::Integrity(_) => {
                    Failure::Integrity(Integrity::Replica)
                }
                _ => Failure::Persistence(Persistence::Replica),
            })?;
        // Declare the registry's schemas so Convergence can classify remote
        // material as interpretable versus opaque.
        let mut supported = replica::body::SupportedSchemas::new();
        for id in self.registry.ids() {
            for reg in self.registry.descriptors(id) {
                for schema in &reg.schemas {
                    let model = match schema.mutation {
                        replica::body::MutationModel::Atomic => replica::body::MUTATION_ATOMIC,
                        replica::body::MutationModel::ImmutableAtomic => {
                            replica::body::MUTATION_IMMUTABLE_ATOMIC
                        }
                        replica::body::MutationModel::Collaborative(_) => {
                            replica::body::MUTATION_COLLABORATIVE
                        }
                    };
                    supported.declare(
                        id.clone(),
                        schema.id.clone(),
                        schema.version,
                        schema.encoding.clone(),
                        model,
                    );
                }
                // Exec truth is Runtime-owned under each World, so these exact
                // schemas are interpretable even though package composition
                // forbids the World from declaring or writing them itself.
                for schema in crate::exec::body_schemas() {
                    let model = match schema.mutation {
                        replica::body::MutationModel::Atomic => replica::body::MUTATION_ATOMIC,
                        replica::body::MutationModel::ImmutableAtomic => {
                            replica::body::MUTATION_IMMUTABLE_ATOMIC
                        }
                        replica::body::MutationModel::Collaborative(_) => {
                            replica::body::MUTATION_COLLABORATIVE
                        }
                    };
                    supported.declare(
                        id.clone(),
                        schema.id,
                        schema.version,
                        schema.encoding,
                        model,
                    );
                }
            }
        }
        replica.set_supported(supported);
        let neighbor_registry = Arc::new(Mutex::new(
            crate::neighbors::NeighborRegistry::load(self.store.dir(), self.store.space())
                .map_err(|_| Failure::Integrity(Integrity::NeighborRegistry))?,
        ));
        let capacity = if options.observation_capacity == 0 {
            crate::session::DEFAULT_OBSERVATION_CAPACITY
        } else {
            options.observation_capacity
        };
        // Corpus images are local acceleration, never Replica authority. A
        // missing/unwrappable cache key or corrupt cache directory disables
        // warm reopen for this activation but cannot block durable truth.
        let corpus_images = crate::corpus_store::CorpusImageStore::open(self.store.dir())
            .ok()
            .map(Arc::new);
        let core = Arc::new(
            crate::session::StationCore::new(
                epoch,
                capacity,
                replica,
                self.read_memory.clone(),
                corpus_images,
            )
            .map_err(|_| Failure::ReadCapacity)?,
        );
        core.set_exec_pacing(options.exec);

        // The resident cache lives beside the store rather than inside it: the
        // journal's promise is that everything a root names is present, and a
        // chunk is optional by design. Mixing them would let a refetchable
        // chunk take a store down.
        let cache = Arc::new(
            replica::content::Residency::open(
                self.store.dir().join(CACHE_DIR),
                options.content.cache_quota_bytes,
            )
            .map_err(|_| Failure::Persistence(Persistence::Cache))?,
        );
        // Nothing was in flight before this Station existed, so every operation
        // lease and every staging slot on disk belongs to a run that is over.
        // Reclaiming here — before anything registers a transfer — is what
        // keeps a killed transfer from holding its chunks resident forever.
        cache
            .sweep_leases(&std::collections::BTreeSet::new())
            .map_err(|_| Failure::Persistence(Persistence::Cache))?;
        cache
            .sweep_staging(&std::collections::BTreeSet::new())
            .map_err(|_| Failure::Persistence(Persistence::Cache))?;
        let content = Arc::new(crate::content_host::ContentHost::new(core.clone(), cache));
        // The oracle over this Station's own content host. Built here rather
        // than inside the plane because a `ContentPolicy` names the Space, the
        // epoch key source and the operator ceiling — all of which belong to the
        // composition root, and a plane that invented its own would be a plane
        // deciding who may be served.
        let residency: Arc<dyn crate::plane::live::ResidencyOracle> =
            Arc::new(crate::plane::live::HostResidency::new(
                content.clone(),
                Arc::new(crate::content_host::StationContentKeys::new(
                    self.keys.clone(),
                )),
                self.store.space().clone(),
                options.content.max_content_len,
            ));
        let live = Arc::new(crate::plane::live::LiveHandle::with_residency(
            Some(core.clone()),
            residency,
        ));

        let station = Station {
            store: self.store,
            registry: self.registry,
            authority: self.authority,
            keys: self.keys,
            read_memory: self.read_memory,
            epoch,
            lock: Some(self.lock),
            alive: Arc::new(AtomicBool::new(true)),
            cancel: CancelToken::new(),
            handles: Mutex::new(Vec::new()),
            drain_deadline,
            core,
            neighbor_registry,
            driver: Mutex::new(None),
            contact_deadline: options
                .comms
                .as_ref()
                .map(|c| c.whole_deadline)
                .unwrap_or(Duration::from_secs(60)),
            content,
            live,
            max_content_len: options.content.max_content_len,
            find_policy: options.find,
        };
        if let Some(comms) = options.comms {
            // Held before `comms` moves into the Contact driver's context: the
            // plane driver needs the same transport and the same identity, and
            // taking them afterwards would mean cloning the whole options
            // struct for two fields.
            let plane_transport = comms.transport.clone();
            let station_seed = comms.station_seed;
            let space = station.store.space().clone();
            let space_bytes = <[u8; 29]>::try_from(space.as_str().as_bytes())
                .map_err(|_| Failure::Integrity(Integrity::SpaceIdentity))?;
            let station_key = mechanics::actor::device_from_seed(&comms.station_seed)
                .key_bytes()
                .ok_or(Failure::Integrity(Integrity::StationKey))?;
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = crate::contact_driver::DriverContext {
                space,
                space_bytes,
                station_key,
                epoch: epoch.as_u64(),
                core: station.core.clone(),
                registry: station.neighbor_registry.clone(),
                authority: station.authority.clone(),
                policy: options.planes.policy(),
                accepted: Mutex::new(crate::admission::AcceptedOpenings::default()),
                options: comms,
                commands: rx,
                cancel: station.cancel.clone(),
            };
            station.spawn_tracked(move |_cancel| crate::contact_driver::run_driver(ctx))?;
            *station.driver.lock_recovering() = Some(tx);

            // The first plane driver in a shipped Station.
            //
            // `lait/freight/1` has had a service since S2 and no mount, so a
            // peer that dialled it completed a handshake and was turned away by
            // the hub — the ALPN was advertised and the plane was not served.
            // Mounting Freight first is deliberate: it proves the wiring where
            // a service already exists and is already tested, so when Live's
            // driver arrives the only new thing is Live.
            //
            // Taking the queue is what makes this exclusive. A second mount for
            // the same plane gets `None` rather than a second reader.
            let local_station =
                Key::from_device(&mechanics::actor::device_from_seed(&station_seed))
                    .ok_or(Failure::Integrity(Integrity::StationKey))?;
            if options.planes.freight_enabled {
                if let Some(queue) = plane_transport.take_session_queue(crate::plane::FREIGHT_ALPN)
                {
                    let context = crate::plane_driver::PlaneContext {
                        plane: crate::plane::Plane::Freight,
                        space: station.store.space().clone(),
                        local_station: local_station.clone(),
                        authority: station.authority.clone(),
                        policy: options.planes.policy(),
                        cancel: station.cancel.clone(),
                        drain_deadline,
                        // A real tick, for the first time. `watch_for_revocation`
                        // has had a live branch since S2 and nothing to watch —
                        // this is rung by every authority advance, so a revoked
                        // peer's session now closes on the change rather than
                        // whenever it happens to disconnect.
                        authority_tick: Some(station.core.authority_tick()),
                    };
                    let service = crate::plane::freight::FreightService::new(
                        station.content.clone(),
                        Arc::new(crate::transfer::TransferRegistry::new()),
                        Arc::new(crate::content_host::StationContentKeys::new(
                            station.keys.clone(),
                        )),
                        station.store.space().clone(),
                        options.content.max_content_len,
                    );
                    // Spawned tracked, so `drain_tasks` joins it rather than
                    // leaving a thread holding a queue after the Station is
                    // gone.
                    station.spawn_tracked(move |_cancel| {
                        crate::plane_driver::run_driver(context, queue, service)
                    })?;
                }
            }

            // The exec plane's driver: opening and admission only in this
            // foundation — an admitted connection is held and every flow is
            // stopped loudly until the typed flow vocabulary lands. Same
            // shared driver, same replay table, same revocation watch.
            if options.planes.exec_enabled {
                if let Some(queue) = plane_transport.take_session_queue(crate::plane::EXEC_ALPN) {
                    let context = crate::plane_driver::PlaneContext {
                        plane: crate::plane::Plane::Exec,
                        space: station.store.space().clone(),
                        local_station: local_station.clone(),
                        authority: station.authority.clone(),
                        policy: options.planes.policy(),
                        cancel: station.cancel.clone(),
                        drain_deadline,
                        authority_tick: Some(station.core.authority_tick()),
                    };
                    let service = crate::plane::exec::Service::new();
                    station.spawn_tracked(move |_cancel| {
                        crate::plane_driver::run_driver(context, queue, service)
                    })?;
                }
            }

            // The second driver, and the reason the queue split had to land
            // first: on one shared queue these two would take strictly
            // alternating connections and each refuse half of what it was
            // handed.
            let dial_station = local_station.clone();
            if options.planes.live_enabled {
                if let Some(queue) = plane_transport.take_session_queue(crate::plane::LIVE_ALPN) {
                    let context = crate::plane_driver::PlaneContext {
                        plane: crate::plane::Plane::Live,
                        space: station.store.space().clone(),
                        local_station,
                        authority: station.authority.clone(),
                        policy: options.planes.policy(),
                        cancel: station.cancel.clone(),
                        drain_deadline,
                        authority_tick: Some(station.core.authority_tick()),
                    };
                    let service = crate::plane::live::LiveService::new(
                        station.live.clone(),
                        station.authority.clone(),
                        station.registry.clone(),
                    );
                    station.spawn_tracked(move |_cancel| {
                        crate::plane_driver::run_driver(context, queue, service)
                    })?;
                }

                // And the outbound half, on its own thread for the same reason
                // the driver has one: a dialled session is served by exactly the
                // same `serve_session` an accepted one is, and that function is
                // not `Send`. Two implementations of a Live session is the way
                // the two quietly stop agreeing.
                //
                // Unconditional on `take_session_queue` succeeding: dialling out
                // is not the same capability as accepting in, and a Station
                // whose inbound queue was already claimed can still reach its
                // neighbours.
                let neighbors = station.neighbor_registry.clone();
                let dial = crate::plane::live::DialContext {
                    space: station.store.space().clone(),
                    local_station: dial_station,
                    transport: plane_transport.clone(),
                    candidates: Arc::new(move || {
                        neighbors
                            .lock_recovering()
                            .snapshot()
                            .into_iter()
                            .map(|neighbor| neighbor.station)
                            .collect()
                    }),
                    handle: station.live.clone(),
                    authority: station.authority.clone(),
                    worlds: station.registry.clone(),
                    cancel: station.cancel.clone(),
                };
                station.spawn_tracked(move |_cancel| crate::plane::live::run_dialer(dial))?;
            }
        }
        Ok(station)
    }

    #[doc(hidden)]
    pub fn open(self, options: Activation) -> Result<Station, Failure> {
        Station::open(self, options)
    }

    /// Destructively remove this local Orbit, consuming it (and its lock). The
    /// confirmation must name this exact Space.
    pub fn remove(self, confirmation: RemovalConfirmation) -> Result<(), Failure> {
        if confirmation.space() != self.store.space() {
            return Err(Failure::Integrity(Integrity::RemovalConfirmation));
        }
        self.store.remove()?;
        // The lock file is gone with the directory; drop the guard.
        drop(self.lock);
        Ok(())
    }
}

/// An advisory, read-only snapshot of a local Orbit. Cannot activate or remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbitStatus {
    pub space: SpaceId,
    /// Whether the Orbit's store is currently locked by an operational owner.
    pub locked: bool,
}

/// What one Orbit's store is holding, as an **observation**.
///
/// Every figure is separately optional because they are separately measurable,
/// and absent means *this was not measured* — never zero, never an estimate. A
/// storage surface that drew an unmeasured footprint as `0 B`, or a store that
/// has never been verified as verified at the epoch, would be making the same
/// claim as one that drew a failed peer sample as "no peers": a confident
/// number where there is no observation at all. Absent is the honest state and
/// this type is shaped so it stays expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageReading {
    /// Bytes on disk under this Orbit's store directory, or `None` when the
    /// walk could not complete. Attributable to this Space alone — the store is
    /// one directory per Space — rather than to the machine.
    pub bytes_on_disk: Option<u64>,
    /// How many Bodies the Replica holds, interpreted and opaque alike.
    pub object_count: Option<u64>,
    /// When the store's material was last verified end to end, in milliseconds
    /// since the unix epoch, or `None` when it never has been. See
    /// [`replica::Replica::verified_at_ms`] for what the verification is.
    pub last_verified_ms: Option<u64>,
}

/// An activated Orbit: the exclusive Replica writer, live task graph, hosted
/// Worlds, docks, and shutdown. **Not** cloneable; [`Station::vacate`] and
/// [`Station::wait`] consume it.
pub struct Station {
    store: OrbitStore,
    registry: Catalog,
    authority: Arc<dyn AuthorityView>,
    keys: Arc<dyn BodyKeySource>,
    read_memory: Arc<crate::session::ReadMemoryGovernor>,
    epoch: Epoch,
    /// The exclusive store lock. `Some` while live; taken out (and either moved
    /// into the returned Orbit or dropped) exactly once at dormancy/exit, so it
    /// is always released last.
    lock: Option<StoreLock>,
    /// Set to `false` to reject new docks and terminate Sessions.
    alive: Arc<AtomicBool>,
    /// Signals tracked tasks to stop.
    cancel: CancelToken,
    /// The one tracked task set.
    handles: Mutex<Vec<JoinHandle<()>>>,
    drain_deadline: Duration,
    /// The exclusive committing state (Replica writer + Observation sequence),
    /// shared with docked Sessions so their commits serialize through the one
    /// Replica. Sessions hold a clone but can never stop the Station.
    core: Arc<crate::session::StationCore>,
    /// The persistent Neighbor registry (loaded at activation).
    neighbor_registry: Arc<Mutex<crate::neighbors::NeighborRegistry>>,
    /// The Contact-plane command channel, when a transport was activated.
    driver: Mutex<Option<std::sync::mpsc::Sender<crate::contact_driver::DriverCmd>>>,
    /// The whole-contact deadline (bounds the administrative contact wait).
    contact_deadline: Duration,
    /// The content plane. Opened at activation beside the Replica, because a
    /// Station that can name content and cannot hold any is a Station whose
    /// content surface is unreachable.
    content: Arc<crate::content_host::ContentHost>,
    /// The Live plane's shared view.
    ///
    /// Held whether or not a driver was mounted. A Station with no transport
    /// still answers "who is here" — with nobody — and a caller that had to
    /// branch on whether the plane exists would write that branch everywhere.
    live: Arc<crate::plane::live::LiveHandle>,
    /// The largest single content this Station will ingest, from operator
    /// policy. Kept here because every local content call has to enforce it and
    /// the options struct does not outlive activation.
    max_content_len: u64,
    /// Local ceilings inherited by every Session admitted here.
    find_policy: crate::find::Policy,
}

impl std::fmt::Debug for Station {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Station")
            .field("space", self.store.space())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl Station {
    /// Open one activation of a vacant Orbit, consuming its operational lease.
    pub fn open(orbit: Orbit, options: Activation) -> Result<Self, Failure> {
        orbit.open_station(options)
    }

    /// This activation's epoch.
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// Watch for durable Exec outbox work. The Station host drains on this
    /// tick; product RPCs do not.
    /// How this activation paces its local Exec drain.
    pub fn exec_pacing(&self) -> ExecPacing {
        self.core.exec_pacing()
    }

    pub fn exec_tick(&self) -> tokio::sync::watch::Receiver<u64> {
        self.core.exec_tick()
    }

    /// What a World declared at registration.
    ///
    /// Narrower than handing out the registry: a caller outside this crate needs
    /// to check a declaration, not to enumerate or resolve Worlds. The registry
    /// is how a Station dispatches, and that is not a composition root's
    /// business.
    pub fn descriptor(&self, world: &WorldId) -> Option<&crate::world::Descriptor> {
        self.registry.descriptor(world)
    }

    /// Resolve the exact World implementation authority selects for this
    /// principal at its current frontier. Application routing uses this before
    /// choosing among multiple installed exact package hosts.
    pub fn active_implementation(
        &self,
        world: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<[u8; 32], Failure> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(Failure::StationDormant);
        }
        let resolution = self
            .authority
            .resolve(identity.device())
            .ok_or(Failure::PrincipalDenied)?;
        self.authority
            .active_implementation(world, &resolution.authority_frontier)
            .map_err(Failure::AuthorityUnavailable)?
            .ok_or_else(|| Failure::NoActiveImplementation(world.clone()))
    }

    /// What this Station currently believes about who is doing what.
    ///
    /// Never durable and never authoritative: a Station with no Live driver
    /// answers with an empty view rather than an error, because "nobody is
    /// here" is the truth about a Station nobody is connected to.
    pub fn live(&self) -> Arc<crate::plane::live::LiveHandle> {
        self.live.clone()
    }

    /// Listen for reliable signals this Station receives.
    ///
    /// Subscribe before anything arrives. A signal is an event, not a state
    /// anyone can re-read, so a listener that attaches late missed what it
    /// missed — the same way a person who was out of the room did.
    pub fn signals(&self) -> tokio::sync::broadcast::Receiver<crate::signal::DeliveredSignal> {
        self.live.signals()
    }

    /// The content plane.
    ///
    /// One per Station and opened at activation, so a caller never constructs
    /// a second cache over the same directory — two caches over one directory
    /// would sweep each other's staging.
    pub fn content(&self) -> Arc<crate::content_host::ContentHost> {
        self.content.clone()
    }

    /// The largest single content this Station will ingest.
    pub fn max_content_len(&self) -> u64 {
        self.max_content_len
    }

    /// What is known about one content.
    pub fn content_stat(
        &self,
        identity: &crate::world::LocalIdentity,
        content: &replica::content::ContentRef,
    ) -> Result<crate::content_host::ContentStatus, crate::content_host::Failure> {
        let keys = self.content_keys();
        let allow = self.content_authorization(identity)?;
        self.content
            .stat(&self.content_policy(&keys, &allow), content)
    }

    /// One bounded range of a content's plaintext.
    pub fn content_read(
        &self,
        identity: &crate::world::LocalIdentity,
        content: &replica::content::ContentRef,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, crate::content_host::Failure> {
        let keys = self.content_keys();
        let allow = self.content_authorization(identity)?;
        self.content
            .read_range(&self.content_policy(&keys, &allow), content, offset, len)
    }

    /// Seal and commit content read from `reader`.
    ///
    /// The reader is consumed incrementally, so a caller forwarding a stream
    /// never holds the whole content — which is the reason this plane exists
    /// rather than an inline field on a Body.
    pub fn content_write(
        &self,
        identity: &crate::world::LocalIdentity,
        operation: [u8; 16],
        reader: &mut dyn std::io::Read,
    ) -> Result<replica::content::ContentRef, crate::content_host::Failure> {
        let keys = self.content_keys();
        let allow = self.content_authorization(identity)?;
        let frontier = self.identity_frontier(identity)?;
        let ctx = replica::transaction::CommitContext {
            space: self.store.space(),
            signer: identity,
            authority_frontier: frontier,
        };
        self.content
            .ingest(&self.content_policy(&keys, &allow), operation, reader, &ctx)
    }

    /// Drop this Station's copy of the bytes and keep the name.
    pub fn content_forget(
        &self,
        identity: &crate::world::LocalIdentity,
        content: &replica::content::ContentRef,
    ) -> Result<(), crate::content_host::Failure> {
        let keys = self.content_keys();
        let allow = self.content_authorization(identity)?;
        self.content
            .remove_local(&self.content_policy(&keys, &allow), content)
    }

    fn content_keys(&self) -> Arc<dyn crate::content_host::ContentKeys> {
        Arc::new(crate::content_host::StationContentKeys::new(
            self.keys.clone(),
        ))
    }

    fn content_policy<'a>(
        &'a self,
        keys: &Arc<dyn crate::content_host::ContentKeys>,
        authorize: &'a dyn for<'c> Fn(
            crate::content_host::ContentAction<'c>,
        ) -> Result<(), Vec<u8>>,
    ) -> crate::content_host::ContentPolicy<'a> {
        crate::content_host::ContentPolicy {
            space: self.store.space(),
            keys: keys.clone(),
            authorize,
            max_content_len: self.max_content_len,
        }
    }

    /// The authority frontier a local identity acts at, or a refusal.
    fn identity_frontier(
        &self,
        identity: &crate::world::LocalIdentity,
    ) -> Result<replica::frontier::AuthorityFrontier, crate::content_host::Failure> {
        self.authority
            .resolve(identity.device())
            .map(|resolved| resolved.authority_frontier)
            .ok_or_else(|| crate::content_host::Failure::Denied {
                demand: b"space.member".to_vec(),
            })
    }

    /// Whether a local identity may act on this Station's content, as a closure
    /// the host can ask per operation.
    ///
    /// Membership is the whole check, and that is a deliberate position rather
    /// than an omission: content authorization is Space-core, so a member may
    /// read any content this Station holds. The consequence is the same one
    /// Freight already carries — residency is Space-wide, and a per-resource
    /// read restriction would be advisory until a `content.serve` grant exists
    /// to make it real. A non-member is refused here, before the store is
    /// touched.
    fn content_authorization(
        &self,
        identity: &crate::world::LocalIdentity,
    ) -> Result<
        impl for<'c> Fn(crate::content_host::ContentAction<'c>) -> Result<(), Vec<u8>>,
        crate::content_host::Failure,
    > {
        self.identity_frontier(identity)?;
        Ok(|_action: crate::content_host::ContentAction<'_>| Ok(()))
    }

    /// The Space this Station serves.
    pub fn space_id(&self) -> &SpaceId {
        self.store.space()
    }

    /// The Station's cancellation token (for spawning tracked tasks).
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Spawn a tracked task. The task receives the [`CancelToken`] and must exit
    /// promptly once it is cancelled. Dormancy drains every tracked task within
    /// the activation's deadline. Refused (with the closure returned) if the
    /// Station is already going dormant.
    pub fn spawn_tracked<F>(&self, f: F) -> Result<(), Failure>
    where
        F: FnOnce(CancelToken) + Send + 'static,
    {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(Failure::StationDormant);
        }
        let token = self.cancel.clone();
        let handle = std::thread::spawn(move || f(token));
        self.handles.lock_recovering().push(handle);
        Ok(())
    }

    /// Attach a local caller to a hosted World and return a [`Session`] bound to
    /// this activation epoch. The caller supplies only a [`LocalIdentity`]
    /// (possession of a device seed) — Runtime **derives** the principal facts by
    /// resolving the device through the mechanics [`AuthorityView`]; a caller
    /// cannot assert actor, membership, or authority frontier. Membership is
    /// re-resolved at every submit, so dock-time facts never outlive the
    /// authority state. Many Sessions may dock; none can stop the Station.
    /// Refused once the Station is going dormant.
    ///
    /// The `station` fact is currently the docking device viewed as a Station id
    /// (local in-process sessions); plumbing the Station's own device identity
    /// through activation arrives with the daemon integration.
    pub fn dock(&self, world_id: &WorldId, identity: &LocalIdentity) -> Result<Session, Failure> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(Failure::StationDormant);
        }
        let resolution = self
            .authority
            .resolve(identity.device())
            .ok_or(Failure::PrincipalDenied)?;
        let station = Key::from_device(identity.device()).ok_or(Failure::PrincipalDenied)?;
        let principal = PrincipalFacts {
            actor: resolution.actor,
            device: identity.device().clone(),
            station,
            space: self.space_id().clone(),
            authority_frontier: resolution.authority_frontier,
        };
        let active_implementation = self
            .authority
            .active_implementation(world_id, &principal.authority_frontier)
            .map_err(Failure::AuthorityUnavailable)?
            .ok_or_else(|| Failure::NoActiveImplementation(world_id.clone()))?;
        let world = self
            .registry
            .world_for(world_id, active_implementation)
            .or_else(|| self.registry.unreviewed_world(world_id))
            .ok_or_else(|| Failure::ImplementationUnavailable {
                world: world_id.clone(),
                implementation: active_implementation,
            })?;
        let registration = self
            .registry
            .descriptor_for(world_id, active_implementation)
            .or_else(|| self.registry.unreviewed_descriptor(world_id))
            .ok_or_else(|| Failure::UnknownWorld(world_id.clone()))?;
        let extractor_schema_digest = crate::publication::ExtractorSchemaDigest::derive(
            &registration.find_schemas,
            &registration.find_extractors,
        )
        .map_err(Failure::InvalidExtractorSchema)?;
        self.core
            .ensure_world_publication(
                &world,
                world_id,
                active_implementation,
                extractor_schema_digest,
                &registration.find_schemas,
                &registration.find_extractors,
            )
            .map_err(|_| Failure::WorldPublicationUnavailable(world_id.clone()))?;
        Ok(Session::new(
            self.store.space().clone(),
            world_id.clone(),
            world,
            identity.clone(),
            principal,
            self.epoch,
            registration.limits,
            registration.schemas.clone(),
            active_implementation,
            extractor_schema_digest,
            self.find_policy,
            self.alive.clone(),
            self.core.clone(),
            self.authority.clone(),
            self.registry.clone(),
        ))
    }

    /// Where this Station's durable state lives.
    ///
    /// Exposed because "nothing was written" is a claim that has to be
    /// checkable, and the only honest way to check it is to look at the bytes.
    /// A frontier can be unchanged across a commit that wrote and then swept,
    /// so a test asserting non-durability against the frontier alone is
    /// asserting less than it thinks.
    pub fn store_dir(&self) -> &std::path::Path {
        self.store.dir()
    }

    /// The current committed Replica frontier (advances as Sessions submit).
    pub fn frontier(&self) -> replica::frontier::ReplicaFrontier {
        self.core.frontier()
    }

    /// What this Station's store is holding: bytes on disk, Bodies, and when
    /// its material was last verified.
    ///
    /// **Passive.** Every figure comes from state this activation already
    /// opened, or from the bytes already sitting in the store directory.
    /// Nothing here places an Orbit, wakes a Station, dials a peer or takes the
    /// store lock — asking what a Station holds must cost what looking costs,
    /// not what opening costs, or a surface listing ten Orbits mounts ten
    /// stores to draw itself.
    pub fn storage(&self) -> StorageReading {
        let (object_count, last_verified_ms) = self.core.storage();
        let bytes_on_disk = match self.store.footprint_bytes() {
            Ok(bytes) => Some(bytes),
            // Absent — not partial, and emphatically not zero. A walk that
            // could not finish measured nothing, and the caller is told that
            // rather than handed a number it would have no way to distrust.
            Err(failure) => {
                tracing::warn!(
                    ?failure,
                    "the Orbit store's footprint could not be measured"
                );
                None
            }
        };
        StorageReading {
            bytes_on_disk,
            object_count: Some(object_count),
            last_verified_ms,
        }
    }

    /// Known/discoverable Neighbors: a consistent snapshot of the persistent
    /// registry (verified Beacon high-water, advisory reachability, retry
    /// state). Reachability is advisory and never standing.
    pub fn neighbors(&self) -> Vec<Neighbor> {
        self.neighbor_registry.lock_recovering().snapshot()
    }

    /// Ingest raw Beacon bytes (e.g. received over an application gossip
    /// surface). Verified, forward-only, coalescing; a fresh advertised
    /// frontier queues a Contact through the Station scheduler.
    pub fn observe_beacon(&self, bytes: &[u8]) {
        if !self.alive.load(Ordering::SeqCst) {
            return;
        }
        let driver = self.driver.lock_recovering();
        if let Some(tx) = driver.as_ref() {
            let _ = tx.send(crate::contact_driver::DriverCmd::Beacon(bytes.to_vec()));
            return;
        }
        drop(driver);
        // No transport: ingest directly into the registry.
        let Ok(signed) = crate::beacon::SignedBeacon::decode_canonical(bytes) else {
            return;
        };
        let Ok(verified) = signed.verify() else {
            return;
        };
        let frontier = self.core.frontier();
        let mut registry = self.neighbor_registry.lock_recovering();
        let _ = registry.observe_beacon(
            &verified,
            (&frontier.root, frontier.transaction_count),
            crate::contact_driver::now_ms(),
            60_000,
        );
    }

    /// An explicitly privileged administrative/test Contact: dial the Neighbor
    /// now (bypassing backoff, not the in-flight bounds), run the full
    /// initiator exchange, validate, and durably incorporate. Not exposed on
    /// ordinary Session handles; refused once the Station is going dormant or
    /// when no transport was activated.
    pub fn contact(
        &self,
        neighbor: &Key,
    ) -> Result<ContactOutcome, crate::plane::contact::Failure> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(crate::plane::contact::Failure::Interrupted);
        }
        let driver = self.driver.lock_recovering();
        let Some(tx) = driver.as_ref() else {
            // Not a wire failure at all, and it used to be indistinguishable
            // from one: no driver means the Station is dormant or was never
            // activated with a transport, and no amount of retrying the peer
            // will change that.
            return Err(crate::plane::contact::Failure::Unreachable(
                "no Contact driver is running for this Station".into(),
            ));
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(crate::contact_driver::DriverCmd::Contact {
            station: neighbor.clone(),
            reply: reply_tx,
        })
        .map_err(|_| {
            crate::plane::contact::Failure::Unreachable(
                "the Contact driver went away mid-request".into(),
            )
        })?;
        drop(driver);
        reply_rx
            .recv_timeout(self.contact_deadline + Duration::from_secs(5))
            .map_err(|_| {
                crate::plane::contact::Failure::Unreachable(
                    "the Contact driver did not answer within its deadline".into(),
                )
            })?
    }

    /// Drain the tracked task set within `deadline`. Returns the join results of
    /// finished tasks and whether any task failed to finish in time.
    fn drain_tasks(&mut self, deadline: Instant) -> (bool, bool) {
        let handles = std::mem::take(&mut *self.handles.lock_recovering());
        loop {
            if handles.iter().all(|h| h.is_finished()) {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let timed_out = handles.iter().any(|h| !h.is_finished());
        let mut any_panicked = false;
        // Reap the finished tasks; never block on an unfinished (rogue) task —
        // it is leaked, and it holds no lock.
        for h in handles {
            if h.is_finished() && h.join().is_err() {
                any_panicked = true;
            }
        }
        (timed_out, any_panicked)
    }

    /// Go dormant, consuming the Station and returning the Orbit. The drain order
    /// is: reject new docks, terminate Sessions, stop scheduling, cancel and
    /// drain tracked tasks within the deadline, checkpoint, and release the store
    /// lock **last**. On a drain timeout the lock is still released and the
    /// durable Orbit remains recoverable via [`Runtime::orbit`].
    pub fn vacate(mut self) -> Result<Orbit, Interruption> {
        // 1) reject new docks + terminate Sessions.
        self.alive.store(false, Ordering::SeqCst);
        // 2) stop scheduling / signal cancellation.
        self.cancel.cancel();
        // 3) cancel and drain tracked tasks within the deadline.
        let deadline = Instant::now() + self.drain_deadline;
        let (timed_out, _panicked) = self.drain_tasks(deadline);
        // 4) close the committing core under the writer mutex — an in-flight
        //    submit either completed its journaled durable commit before the
        //    close or observes it and is refused. Every acknowledged commit is
        //    already on disk (the journal protocol ran at commit time), so
        //    dormancy needs no separate checkpoint.
        self.core.close();
        // 5) build the recovered Orbit and release the lock last.
        #[allow(
            clippy::expect_used,
            reason = "vacate consumes a Station whose lock is installed by its only constructor"
        )]
        let lock = self.lock.take().expect("station holds its lock");
        if timed_out {
            // The lock releases here; the store persists and is re-acquirable.
            drop(lock);
            return Err(Interruption::DrainTimeout);
        }
        Ok(Orbit::new(
            self.store,
            self.registry,
            self.authority,
            self.keys,
            self.read_memory,
            self.epoch,
            lock,
        ))
    }

    /// Park until every tracked task exits, consuming the Station and returning a
    /// recoverable [`Exit`]. A task panic is reported as the exit reason;
    /// the durable Orbit is recovered either way and the lock is released last.
    /// No commit is lost on this path: every acknowledged commit was already
    /// durably written by the per-commit sink, and the core is closed (under the
    /// writer mutex) before the Orbit is returned.
    pub fn wait(mut self) -> Exit {
        let handles = std::mem::take(&mut *self.handles.lock_recovering());
        let mut reason = None;
        for h in handles {
            if h.join().is_err() {
                reason = Some(ExitReason::TaskFailed);
            }
        }
        self.alive.store(false, Ordering::SeqCst);
        self.core.close();
        #[allow(
            clippy::expect_used,
            reason = "wait consumes a Station whose lock is installed by its only constructor"
        )]
        let lock = self.lock.take().expect("station holds its lock");
        Exit {
            orbit: Orbit::new(
                self.store,
                self.registry,
                self.authority,
                self.keys,
                self.read_memory,
                self.epoch,
                lock,
            ),
            reason,
        }
    }
}

/// Another known or discoverable Station. Neighbor state is keyed by verified
/// [`Key`]; reachability is advisory and never standing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbor {
    pub station: Key,
    pub reachability: Reachability,
    /// When this Neighbor was last heard from (ms since the unix epoch,
    /// receiver-local; 0 = never observed live). Advisory.
    pub last_seen_ms: u64,
    // The scheduler's own inputs, carried out so somebody can ask *why* a
    // Neighbor is not being dialed. `NeighborRegistry::eligible` reads exactly
    // these three; without them a stalled node is indistinguishable from an
    // idle one from the outside, which is precisely the state that took six
    // rounds of manual probing to identify once.
    /// Whether anything has queued a Contact with this Neighbor.
    pub pending: bool,
    /// Backoff floor: no attempt before this (ms since the unix epoch).
    pub next_attempt_ms: u64,
    /// The furthest route-lease expiry held for this Neighbor (0 = no route).
    /// An expired lease suppresses dialing even when the backoff is due.
    pub route_lease_expires_ms: u64,
    /// Consecutive failed Contacts. Drives the backoff.
    pub failures: u32,
}

/// Advisory reachability of a Neighbor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    Unknown,
    Reachable,
    Unreachable,
}

/// The outcome of a Contact: bytes moved reported **separately** from the
/// Convergence classification of the material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactOutcome {
    pub bytes_moved: u64,
    pub convergence: ConvergenceOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    pub(crate) fn test_keys() -> Arc<dyn BodyKeySource> {
        Arc::new(replica::body::StaticBodyKeys::new(
            mechanics::authorization::AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
        ))
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lait-runtime-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn runtime(root: &std::path::Path) -> Runtime {
        // These lifecycle tests never dock, so the deny-all authority suffices.
        Runtime::open(
            root.to_path_buf(),
            Builder::new().build().unwrap(),
            Arc::new(DenyAllAuthority),
            test_keys(),
        )
    }

    #[test]
    fn form_drop_and_reacquire_an_existing_orbit() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        // Dropping the Orbit releases the lock.
        drop(orbit);
        // The durable Orbit is re-acquirable.
        let again = rt.acquire(&space).unwrap();
        assert_eq!(again.space_id(), &space);
    }

    #[test]
    fn observation_is_advisory_and_cannot_operate() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        let station = orbit.open(Activation::default()).unwrap();
        // Observation sees the Orbit and reports it locked, but yields no handle
        // that can activate or remove (it is a plain data snapshot).
        let obs = rt.inspect(&space).unwrap();
        assert_eq!(obs.space, space);
        assert!(obs.locked, "an active Station holds the lock");
        drop(station);
    }

    #[test]
    fn activation_consumes_orbit_bumps_epoch_and_dormancy_returns_it() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        assert_eq!(orbit.epoch(), Epoch::ZERO);

        let station = orbit.open(Activation::default()).unwrap();
        assert_eq!(station.epoch(), Epoch::from_u64(1));

        let orbit = station.vacate().unwrap();
        assert_eq!(orbit.space_id(), &space);
        // A second activation advances the durable epoch again.
        let station = orbit.open(Activation::default()).unwrap();
        assert_eq!(station.epoch(), Epoch::from_u64(2));
        drop(station);
    }

    #[test]
    fn a_second_acquisition_is_a_typed_double_lock() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        let station = orbit.open(Activation::default()).unwrap();
        // While the Station holds the lock, a second acquisition is refused.
        assert!(matches!(rt.acquire(&space), Err(Failure::ReplicaLocked(_))));
        drop(station);
    }

    #[test]
    fn no_task_or_handle_retains_the_lock_after_exit() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        let station = orbit.open(Activation::default()).unwrap();
        // A cooperative tracked task that finishes on cancellation.
        station
            .spawn_tracked(|cancel| {
                while !cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
            .unwrap();
        let orbit = station.vacate().unwrap();
        drop(orbit);
        // The lock is free again.
        assert!(rt.acquire(&space).is_ok());
    }

    #[test]
    fn a_rogue_task_times_out_but_still_releases_the_lock() {
        let root = temp_root();
        let rt = runtime(&root);
        let stop = Arc::new(AtomicBool::new(false));
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        let opts = Activation {
            exec: Default::default(),
            drain_deadline: Duration::from_millis(20),
            ..Default::default()
        };
        let station = orbit.open(opts).unwrap();
        let stop2 = stop.clone();
        // A task that ignores cancellation until we let it go.
        station
            .spawn_tracked(move |_cancel| {
                while !stop2.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
            .unwrap();
        assert!(matches!(station.vacate(), Err(Interruption::DrainTimeout)));
        // Despite the timeout, the store lock was released and the Orbit is
        // recoverable.
        assert!(rt.acquire(&space).is_ok());
        stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn deorbit_removes_the_store() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        orbit
            .remove(RemovalConfirmation::for_space(space.clone()))
            .unwrap();
        assert!(matches!(rt.acquire(&space), Err(Failure::OrbitNotFound(_))));
    }

    #[test]
    fn deorbit_confirmation_must_name_the_same_space() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        // A confirmation for a *different* Space is refused, and the store is
        // left intact (the confirmation binds removal to an exact Space).
        let wrong = RemovalConfirmation::for_space(SpaceId::from_digest([0xEE; 16]));
        assert!(matches!(
            orbit.remove(wrong),
            Err(Failure::Integrity(Integrity::RemovalConfirmation))
        ));
        assert!(
            rt.acquire(&space).is_ok(),
            "store survived the refused remove"
        );
    }

    #[test]
    fn wait_returns_a_recoverable_orbit() {
        let root = temp_root();
        let rt = runtime(&root);
        let orbit = rt.create().unwrap();
        let space = orbit.space_id().clone();
        let station = orbit.open(Activation::default()).unwrap();
        // A task that exits on its own.
        station.spawn_tracked(|_cancel| {}).unwrap();
        let exit = station.wait();
        assert_eq!(exit.orbit.space_id(), &space);
        assert!(exit.reason.is_none());
    }

    #[test]
    fn a_runtime_without_a_root_cannot_form() {
        let rt = Runtime::from_registry(Builder::new().build().unwrap());
        assert!(matches!(rt.create(), Err(Failure::NoStoreRoot)));
        assert!(rt.list().is_empty());
    }
}
