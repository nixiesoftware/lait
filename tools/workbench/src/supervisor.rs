use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, Mutex, RwLock};

use crate::contract::{
    BackendEvent, Capabilities, ClientSignal, ConnectionEvent, ConnectionEventKind,
    ConnectionHistoryPage, ConnectionSnapshot, DeviceFacts, DeviceSnapshot, EnvironmentSnapshot,
    EventHistoryPage, EventKind, HistoryQuery, ImageFacts, LifecycleState, LogPage,
    ObservationHealth, ObservationState, RemoveDeviceRequest, SnapshotReason, UpdateDeviceRequest,
    WorkbenchSnapshot, SCHEMA_VERSION,
};
use crate::driver::{DaemonDriver, DaemonProbe, LaitDriver, OwnedDaemon};
use crate::heads::{start_browser, HeadFacts, OwnedHead};
use crate::observability::{read_log_page, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT};
use crate::registry::{RegisteredDevice, Registry};
use crate::staging::{StagedImage, Staging};

const START_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_HISTORY_CAPACITY: usize = 1_024;
const CONNECTION_HISTORY_CAPACITY: usize = 2_048;

/// How often a started supervisor samples the daemons it can see.
///
/// Observation is passive: it reads what is already there and never places,
/// mounts or wakes anything to answer.
pub const OBSERVATION_INTERVAL: Duration = Duration::from_secs(1);

type ConnectionKey = (String, String, String);

/// What a supervisor needs to exist.
///
/// Everything here was previously read from the environment inside a `main`,
/// which made an embedding consumer inherit a process-wide contract it never
/// asked for. A caller now states it.
#[derive(Clone, Debug)]
pub struct Config {
    /// The managed root every device home is created beneath. It is also the
    /// containment boundary: a path that does not canonicalize to somewhere
    /// under it is refused rather than touched.
    pub state_root: PathBuf,
    /// The `lait` image devices are spawned from.
    pub executable: PathBuf,
    /// How often to sample. [`OBSERVATION_INTERVAL`] unless a caller has a
    /// reason — a test wanting a tighter loop, most likely.
    pub observation_interval: Duration,
    /// Whether daemons run from the executable in place or from a staged copy.
    /// A packaged client wants [`Staging::Direct`]; a development run wants a
    /// per-run [`Staging::Staged`] root, so a rebuild never contends with a
    /// process holding the image.
    pub staging: Staging,
}

impl Config {
    /// A configuration sampling at [`OBSERVATION_INTERVAL`].
    pub fn new(state_root: PathBuf, executable: PathBuf) -> Self {
        Self {
            state_root,
            executable,
            observation_interval: OBSERVATION_INTERVAL,
            staging: Staging::Direct,
        }
    }

    /// Run daemons from a staged copy beneath `root`.
    pub fn staged_in(mut self, root: PathBuf) -> Self {
        self.staging = Staging::Staged { root };
        self
    }
}

#[derive(Debug)]
pub enum SupervisorError {
    Invalid(String),
    AlreadyExists(String),
    NotFound(String),
    Conflict(String),
    Internal(anyhow::Error),
}

impl SupervisorError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_request",
            Self::AlreadyExists(_) => "already_exists",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "lifecycle_conflict",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message)
            | Self::AlreadyExists(message)
            | Self::NotFound(message)
            | Self::Conflict(message) => formatter.write_str(message),
            Self::Internal(error) => write!(formatter, "{error:#}"),
        }
    }
}

impl std::error::Error for SupervisorError {}

impl From<anyhow::Error> for SupervisorError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Inner>,
}

struct Inner {
    state_root: PathBuf,
    executable: PathBuf,
    /// The image devices are actually spawned from, which is the staged copy
    /// when staging is on. `None` when nothing has been staged — a fake driver
    /// in a test, where there is no real executable to copy.
    image: StdMutex<Option<ImageFacts>>,
    driver: Arc<dyn DaemonDriver>,
    registry: Registry,
    devices: RwLock<BTreeMap<String, Arc<Device>>>,
    revision: AtomicU64,
    signals: broadcast::Sender<ClientSignal>,
    history: StdMutex<HistoryState>,
    observed_connections: StdMutex<BTreeMap<ConnectionKey, ConnectionSnapshot>>,
    observed_devices: StdMutex<BTreeMap<String, DeviceObservation>>,
    observed_log_sizes: StdMutex<BTreeMap<String, u64>>,
    observation: Mutex<()>,
    observer: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    /// The browser heads this run started, by id. External heads are not here
    /// and never will be — an MCP head is spawned by an agent's harness, so
    /// there is no handle for this map to hold.
    heads: StdMutex<BTreeMap<String, OwnedHead>>,
    /// Head keys with a spawn in flight.
    ///
    /// The map alone cannot exclude a second spawn, because `spawn_head` checks it,
    /// then releases the lock for up to `READY_TIMEOUT` while a process starts and
    /// announces itself, then inserts. Two concurrent asks for one World both passed
    /// the check, both started a real head, and the second `insert` dropped the first
    /// `OwnedHead` — which neither kills nor waits, so it became an orphan holding a
    /// port and a live run credential, unlistable and unstoppable.
    ///
    /// A reservation taken under the same lock as the check closes the window
    /// without holding a lock across the wait. This is what actually prevents two
    /// heads for one World; keying by mount was necessary and never sufficient.
    starting: StdMutex<std::collections::BTreeSet<String>>,
    start_timeout: Duration,
    stop_timeout: Duration,
    /// The configuration this supervisor was started with, kept so
    /// [`Supervisor::reload_in_place`] can restage from the same source under
    /// the same policy. `None` for a supervisor constructed bare
    /// ([`Supervisor::new`]) — there was no staging to repeat, and an
    /// in-place reload is refused rather than guessed at.
    reload_config: StdMutex<Option<Config>>,
}

/// Holds a head key's spawn reservation, and gives it back however the spawn ends.
///
/// A guard rather than a `remove` at each exit, because `spawn_head` has several —
/// the announce timeout, a spawn error, and success — and the one that would be
/// forgotten is the one that matters: a reservation left behind by a failed spawn
/// makes that World unstartable for the life of the process.
struct Reservation<'a> {
    starting: &'a StdMutex<std::collections::BTreeSet<String>>,
    id: String,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        lock_recovering(self.starting).remove(&self.id);
    }
}

/// The sampler is owned by the supervisor it samples, so it cannot outlive one.
///
/// `Supervisor` is a handle and clones share this `Inner`; the task therefore
/// stops when the *last* handle goes, not when the first one does. That is why
/// stopping it lives here rather than in a `Drop` on the handle itself.
impl Drop for Inner {
    fn drop(&mut self) {
        // Taken in its own statement so the guard is released before the abort,
        // rather than living to the end of the `if let`.
        let observer = lock_recovering(&self.observer).take();
        if let Some(observer) = observer {
            observer.abort();
        }
    }
}

struct Device {
    id: String,
    /// Behind a lock because a rename changes it while the rest of the device —
    /// its home, and above all the owned process handle in `runtime` — must
    /// survive untouched. Rebuilding the `Arc` to change one string would strand
    /// the handle that proves this supervisor spawned the daemon.
    label: StdMutex<String>,
    home: PathBuf,
    runtime: Mutex<Runtime>,
}

impl Device {
    fn label(&self) -> String {
        lock_recovering(&self.label).clone()
    }

    fn registration(&self) -> RegisteredDevice {
        RegisteredDevice {
            id: self.id.clone(),
            label: self.label(),
        }
    }
}

/// Every registration currently held, in id order — the exact bytes the registry
/// is asked to persist.
fn registrations(devices: &BTreeMap<String, Arc<Device>>) -> Vec<RegisteredDevice> {
    devices
        .values()
        .map(|device| device.registration())
        .collect()
}

struct Runtime {
    state: LifecycleState,
    process: Option<Box<dyn OwnedDaemon>>,
    started_at_ms: Option<u64>,
    last_error: Option<String>,
}

/// What a reload did, said plainly enough to show a person.
///
/// `left_running` carries a reason per device rather than a count: "two daemons
/// were left running" is not something anybody can act on, and the reason is
/// always the same shape — the evidence to stop it was not there.
#[derive(Clone, Debug, Default)]
pub struct Reload {
    pub was_running: Vec<String>,
    pub was_external: Vec<String>,
    pub stopped: Vec<String>,
    pub left_running: Vec<(String, String)>,
    pub restarted: Vec<String>,
    pub image: Option<ImageFacts>,
}

/// One consumer's cursor into the supervisor's signal stream.
///
/// This is deliberately not a `Stream` implementation: the consumer is Rust
/// calling a library directly, `recv` is the whole interface, and keeping it
/// that way is what lets the core link no stream combinator crate at all.
pub struct Signals {
    receiver: broadcast::Receiver<ClientSignal>,
}

impl Signals {
    /// The next signal, or `None` once the supervisor is gone and the stream
    /// can produce nothing further.
    ///
    /// A lagged consumer is handed [`SnapshotReason::ConsumerLagged`] *here*,
    /// in sequence, rather than having the loss reported out of band or not at
    /// all. That placement is the whole reason this type exists: the consumer
    /// learns it lost events at exactly the point in the stream where it lost
    /// them, so it knows precisely what its snapshot has to cover.
    pub async fn recv(&mut self) -> Option<ClientSignal> {
        match self.receiver.recv().await {
            Ok(signal) => Some(signal),
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                Some(ClientSignal::lagged(dropped))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// What sampling last learned about a device, and whether it is current.
///
/// Kept beside `Runtime` rather than inside it because the two answer different
/// questions: `Runtime` is what this supervisor *did* to a process and is
/// authoritative by construction, while this is what a daemon *said* and can go
/// stale without anything here changing.
#[derive(Clone, Default)]
struct DeviceObservation {
    facts: Option<DeviceFacts>,
    health: ObservationHealth,
    /// The proven identity of an external daemon serving this device, recorded
    /// while it could be observed. `stop_external` requires it and re-proves it;
    /// nothing is ever stopped on a pid alone.
    identity: Option<lait::daemon_spawn::ProcessIdentity>,
}

#[derive(Default)]
struct HistoryState {
    events: VecDeque<BackendEvent>,
    connections: VecDeque<ConnectionEvent>,
    dropped_events_through: Option<u64>,
    dropped_connections_through: Option<u64>,
}

impl Supervisor {
    /// Construct a supervisor and begin observing.
    ///
    /// This is the call an embedding consumer makes. It takes the first
    /// observation before it returns, so the supervisor is already
    /// authoritative rather than empty when the caller first reads it — an
    /// empty first snapshot is indistinguishable from a machine with no
    /// daemons on it, and the difference matters.
    ///
    /// End it with [`Supervisor::shutdown`]. Dropping every handle without
    /// that stops the sampling but leaves owned daemons running, which is the
    /// correct reading of a consumer that crashed: those daemons come back as
    /// `External` to whoever supervises next, and no new supervisor claims an
    /// owned handle it cannot prove it created.
    ///
    /// # Why this returns the stream too
    ///
    /// The signal stream must be established *before* the first observation, or
    /// the window between them drops events silently — the consumer's snapshot
    /// would be older than its first signal and nothing would say so. Returning
    /// both together makes that ordering structural: there is no way to hold a
    /// started supervisor and not already hold a stream that began before it
    /// observed anything.
    pub async fn start(config: Config) -> Result<(Self, Signals), SupervisorError> {
        let image = StagedImage::prepare(&config.executable, &config.staging, now_ms())
            .map_err(SupervisorError::Internal)?;
        // The staged copy is what devices are spawned from, so it is what the
        // driver is built with. `ImageFacts` keeps the source path, which is
        // the only reason the two can be told apart later.
        let supervisor = Self::new(config.state_root.clone(), image.executable().to_owned())?;
        *lock_recovering(&supervisor.inner.image) = Some(image.facts().clone());
        *lock_recovering(&supervisor.inner.reload_config) = Some(config.clone());
        let signals = supervisor.signals();
        supervisor.observe().await;
        supervisor.observe_in_background(config.observation_interval);
        Ok((supervisor, signals))
    }

    /// Sample on `interval` until the last handle to this supervisor is gone.
    ///
    /// The caller has already taken the first observation.
    fn observe_in_background(&self, interval: Duration) {
        // Weak, and deliberately: a strong handle here would be a cycle — the
        // task would hold the `Inner` that holds the task's own join handle, so
        // the refcount could never reach zero, `Drop` could never run, and the
        // sampler would outlive its supervisor for the life of the process.
        // Upgrading per tick also gives the loop its own exit: once the last
        // real handle is gone there is nothing left to observe.
        let sampled = Arc::downgrade(&self.inner);
        let observer = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The first tick completes immediately, and `start` has already
            // taken that observation.
            ticks.tick().await;
            loop {
                ticks.tick().await;
                let Some(inner) = sampled.upgrade() else {
                    return;
                };
                // Dropped before the next tick is awaited, so the supervisor is
                // held only for as long as observing it takes.
                Supervisor { inner }.observe().await;
            }
        });
        *lock_recovering(&self.inner.observer) = Some(observer);
    }

    /// Stop observing, then stop every daemon this supervisor owns.
    ///
    /// In that order: a sampler still running while daemons come down races
    /// each stop and publishes lifecycle events for transitions that are
    /// already accounted for. Externally discovered daemons are left running,
    /// as they are on every other path.
    pub async fn shutdown(&self) {
        let observer = lock_recovering(&self.inner.observer).take();
        if let Some(observer) = observer {
            observer.abort();
        }
        // Heads first. A head outliving the supervisor that started it is an
        // orphan holding a port and the image, answering to nobody — the exact
        // shape this initiative exists to stop producing.
        let heads: Vec<String> = lock_recovering(&self.inner.heads).keys().cloned().collect();
        for id in heads {
            let _ = self.stop_head(&id).await;
        }
        self.stop_all_owned().await;
    }

    /// Construct a supervisor without starting the background observation.
    ///
    /// [`Supervisor::start`] is what a consumer wants. This is for a caller
    /// that drives [`Supervisor::observe`] itself, which in practice means a
    /// test that needs sampling to happen at points it chooses.
    pub fn new(state_root: PathBuf, executable: PathBuf) -> Result<Self, SupervisorError> {
        std::fs::create_dir_all(&state_root).map_err(|error| {
            SupervisorError::Internal(anyhow::anyhow!(
                "create workbench state root {}: {error}",
                state_root.display()
            ))
        })?;
        let state_root = std::fs::canonicalize(&state_root).map_err(|error| {
            SupervisorError::Internal(anyhow::anyhow!(
                "canonicalize workbench state root {}: {error}",
                state_root.display()
            ))
        })?;
        let driver = Arc::new(LaitDriver::new(executable.clone()));
        Self::with_driver(state_root, executable, driver, START_TIMEOUT, STOP_TIMEOUT)
    }

    fn with_driver(
        state_root: PathBuf,
        executable: PathBuf,
        driver: Arc<dyn DaemonDriver>,
        start_timeout: Duration,
        stop_timeout: Duration,
    ) -> Result<Self, SupervisorError> {
        let registry = Registry::open(&state_root)?;
        let mut devices = BTreeMap::new();
        for registration in registry.load()? {
            validate_device_id(&registration.id)?;
            validate_label(&registration.label)?;
            let id = registration.id.clone();
            if devices.contains_key(&id) {
                return Err(SupervisorError::Invalid(format!(
                    "workbench registry contains duplicate device '{id}'"
                )));
            }
            devices.insert(id, open_device(&state_root, registration)?);
        }
        let (signals, _) = broadcast::channel(256);
        Ok(Self {
            inner: Arc::new(Inner {
                state_root,
                executable,
                image: StdMutex::new(None),
                driver,
                registry,
                devices: RwLock::new(devices),
                revision: AtomicU64::new(0),
                signals,
                history: StdMutex::new(HistoryState::default()),
                observed_connections: StdMutex::new(BTreeMap::new()),
                observed_devices: StdMutex::new(BTreeMap::new()),
                observed_log_sizes: StdMutex::new(BTreeMap::new()),
                observation: Mutex::new(()),
                observer: StdMutex::new(None),
                heads: StdMutex::new(BTreeMap::new()),
                starting: StdMutex::new(std::collections::BTreeSet::new()),
                start_timeout,
                stop_timeout,
                reload_config: StdMutex::new(None),
            }),
        })
    }

    /// Attach to the one ordered signal stream.
    ///
    /// Every consumer gets its own cursor into the same sequence. A consumer
    /// that falls behind is told so *at the point it fell behind*, rather than
    /// silently skipping ahead.
    pub fn signals(&self) -> Signals {
        Signals {
            receiver: self.subscribe(),
        }
    }

    /// The raw receiver behind [`Supervisor::signals`], for an adapter that
    /// already has its own stream machinery. Lag arrives as
    /// `RecvError::Lagged`; map it with [`ClientSignal::lagged`] rather than
    /// inventing a second spelling of it.
    pub fn subscribe(&self) -> broadcast::Receiver<ClientSignal> {
        self.inner.signals.subscribe()
    }

    /// Reconcile persisted definitions with the control sockets that exist now.
    ///
    /// A daemon surviving a workbench crash is deliberately `External`: the
    /// new process has no owned child handle and therefore no force-stop right.
    pub async fn reconcile(&self) {
        let devices: Vec<Arc<Device>> = self.inner.devices.read().await.values().cloned().collect();
        for device in devices {
            let mut runtime = device.runtime.lock().await;
            if runtime.process.is_some() {
                continue;
            }
            let previous = runtime.state;
            match self.inner.driver.probe(&device.home).await {
                DaemonProbe::Healthy => {
                    runtime.state = LifecycleState::External;
                    runtime.last_error = None;
                }
                DaemonProbe::Absent => {
                    if runtime.state == LifecycleState::External {
                        runtime.state = LifecycleState::Stopped;
                        runtime.started_at_ms = None;
                        runtime.last_error = None;
                    }
                }
                DaemonProbe::Foreign { why, replaceable } => {
                    runtime.state = LifecycleState::Failed;
                    runtime.last_error = Some(if replaceable {
                        format!("incompatible daemon can be explicitly replaced: {why}")
                    } else {
                        format!("incompatible daemon cannot be safely replaced: {why}")
                    });
                }
            }
            if runtime.state != previous {
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(device.id.clone()),
                    "persisted device state reconciled",
                );
            }
        }
    }

    /// Refresh passive observability state without taking ownership of a daemon.
    ///
    /// Calls are serialized because both the HTTP snapshot path and the
    /// background sampler may request a refresh at the same time.
    pub async fn observe(&self) {
        let _observation = self.inner.observation.lock().await;
        self.reconcile().await;
        let devices: Vec<Arc<Device>> = self.inner.devices.read().await.values().cloned().collect();
        let at_ms = now_ms();
        let mut current_connections = BTreeMap::new();
        let mut current_log_sizes = BTreeMap::new();
        // The devices whose peers this pass actually read. A device missing from
        // this set keeps whatever was last seen for it rather than being
        // rewritten to nothing.
        let mut sampled: Vec<String> = Vec::new();
        for device in devices {
            let mut runtime = device.runtime.lock().await;
            self.refresh_owned_process(&device, &mut runtime);
            let inspect = matches!(
                runtime.state,
                LifecycleState::Running | LifecycleState::External
            );
            let owned = runtime.process.is_some();
            drop(runtime);

            if !inspect {
                // A stopped device is not a failed observation. There is nothing
                // to ask, the answer is genuinely nothing, and its health goes
                // back to healthy-with-no-sample rather than staying degraded
                // from whenever it was last up.
                self.record_device_observation(&device.id, Ok(None), at_ms);
                sampled.push(device.id.clone());
                current_log_sizes.insert(device.id.clone(), log_size(&device));
                continue;
            }

            // Only an unowned daemon needs a proven identity: an owned one is
            // held by a handle, which is stronger evidence than anything that
            // can be read back about it. Recorded while it can be observed,
            // because the moment somebody wants to stop it may be the moment it
            // has stopped answering.
            if !owned {
                let identity = self.inner.driver.identity(&device.home).await.ok();
                lock_recovering(&self.inner.observed_devices)
                    .entry(device.id.clone())
                    .or_default()
                    .identity = identity;
            }

            let facts = self.inner.driver.facts(&device.home).await;
            let peers = self.inner.driver.connections(&device.home).await;
            match (facts, peers) {
                (Ok(facts), Ok(peers)) => {
                    self.record_device_observation(&device.id, Ok(Some(facts)), at_ms);
                    sampled.push(device.id.clone());
                    for peer in peers {
                        let snapshot = ConnectionSnapshot {
                            source_device_id: device.id.clone(),
                            space_id: peer.space_id,
                            peer_id: peer.peer_id,
                            peer_nick: peer.peer_nick,
                            state: peer.state,
                            online: peer.online,
                            dialable: peer.dialable,
                            blocked_by: peer.blocked_by,
                            target_device_id: None,
                        };
                        current_connections.insert(connection_key(&snapshot), snapshot);
                    }
                }
                (facts, peers) => {
                    // Either half failing degrades the device and leaves its
                    // last good topology standing. Reporting half a sample as a
                    // whole one is how a surface starts showing a peer count
                    // that quietly stopped being true.
                    let error = facts.err().or_else(|| peers.err()).map_or_else(
                        || "observation failed".to_owned(),
                        |error| format!("{error:#}"),
                    );
                    self.record_device_observation(&device.id, Err(error), at_ms);
                }
            }

            current_log_sizes.insert(device.id.clone(), log_size(&device));
        }
        let current_connections = self.correlate_managed_peers(current_connections);
        self.record_connection_observations(current_connections, &sampled);
        self.record_log_observations(current_log_sizes);
    }

    /// Fill in `target_device_id` wherever an observed peer is a device this
    /// supervisor manages.
    ///
    /// Correlation is by Station id, which a daemon reports for itself, and
    /// never by nickname — a nick is authored and not unique, so matching on it
    /// would let one device's label name another device's row.
    fn correlate_managed_peers(
        &self,
        mut connections: BTreeMap<ConnectionKey, ConnectionSnapshot>,
    ) -> BTreeMap<ConnectionKey, ConnectionSnapshot> {
        let stations: BTreeMap<String, String> = lock_recovering(&self.inner.observed_devices)
            .iter()
            .filter_map(|(device_id, observation)| {
                let station = observation.facts.as_ref()?.station_id.clone()?;
                Some((station, device_id.clone()))
            })
            .collect();
        if stations.is_empty() {
            return connections;
        }
        for connection in connections.values_mut() {
            connection.target_device_id = stations.get(&connection.peer_id).cloned();
        }
        connections
    }

    /// Record one device's sampling outcome.
    ///
    /// `Ok(Some(facts))` is a successful read, `Ok(None)` is "there was nothing
    /// to read and that is not a failure", and `Err` degrades the device while
    /// leaving the last good facts in place.
    fn record_device_observation(
        &self,
        device_id: &str,
        outcome: Result<Option<DeviceFacts>, String>,
        at_ms: u64,
    ) {
        let mut observed = lock_recovering(&self.inner.observed_devices);
        let entry = observed.entry(device_id.to_owned()).or_default();
        match outcome {
            Ok(facts) => {
                if let Some(facts) = facts {
                    entry.facts = Some(facts);
                }
                entry.health = ObservationHealth {
                    state: ObservationState::Healthy,
                    sampled_at_ms: Some(at_ms),
                    stale_since_ms: None,
                    error: None,
                };
            }
            Err(error) => {
                entry.health = ObservationHealth {
                    state: ObservationState::Degraded,
                    // Preserved: it says how old the surviving figures are.
                    sampled_at_ms: entry.health.sampled_at_ms,
                    // The *start* of the degraded stretch, not this attempt, so
                    // repeated failures do not keep resetting how stale it is.
                    stale_since_ms: entry.health.stale_since_ms.or(Some(at_ms)),
                    error: Some(error),
                };
            }
        }
    }

    fn observation_of(&self, device_id: &str) -> DeviceObservation {
        lock_recovering(&self.inner.observed_devices)
            .get(device_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn event_history(&self, query: &HistoryQuery) -> Result<EventHistoryPage, SupervisorError> {
        let limit = history_limit(query.limit)?;
        let after = query.after_revision.unwrap_or(0);
        let history = lock_recovering(&self.inner.history);
        let oldest = history.events.front().map(|event| event.revision);
        let dropped_before = history
            .dropped_events_through
            .is_some_and(|revision| after <= revision);
        let mut matching: Vec<BackendEvent> = history
            .events
            .iter()
            .filter(|event| event.revision > after)
            .filter(|event| {
                query
                    .device_id
                    .as_ref()
                    .is_none_or(|device_id| event.device_id.as_ref() == Some(device_id))
            })
            .take(limit.saturating_add(1))
            .cloned()
            .collect();
        drop(history);
        let has_more = matching.len() > limit;
        matching.truncate(limit);
        let next_revision = matching.last().map_or(after, |event| event.revision);
        Ok(EventHistoryPage {
            schema_version: SCHEMA_VERSION,
            oldest_available_revision: oldest,
            newest_revision: self.inner.revision.load(Ordering::SeqCst),
            next_revision,
            dropped_before,
            has_more,
            events: matching,
        })
    }

    pub fn connection_history(
        &self,
        query: &HistoryQuery,
    ) -> Result<ConnectionHistoryPage, SupervisorError> {
        let limit = history_limit(query.limit)?;
        let after = query.after_revision.unwrap_or(0);
        let history = lock_recovering(&self.inner.history);
        let oldest = history.connections.front().map(|event| event.revision);
        let dropped_before = history
            .dropped_connections_through
            .is_some_and(|revision| after <= revision);
        let mut matching: Vec<ConnectionEvent> = history
            .connections
            .iter()
            .filter(|event| event.revision > after)
            .filter(|event| {
                query
                    .device_id
                    .as_ref()
                    .is_none_or(|value| event.connection.source_device_id == *value)
                    && query
                        .space_id
                        .as_ref()
                        .is_none_or(|value| event.connection.space_id == *value)
                    && query
                        .peer_id
                        .as_ref()
                        .is_none_or(|value| event.connection.peer_id == *value)
            })
            .take(limit.saturating_add(1))
            .cloned()
            .collect();
        drop(history);
        let has_more = matching.len() > limit;
        matching.truncate(limit);
        let next_revision = matching.last().map_or(after, |event| event.revision);
        Ok(ConnectionHistoryPage {
            schema_version: SCHEMA_VERSION,
            oldest_available_revision: oldest,
            newest_revision: self.inner.revision.load(Ordering::SeqCst),
            next_revision,
            dropped_before,
            has_more,
            events: matching,
        })
    }

    pub async fn logs(
        &self,
        id: &str,
        cursor: Option<u64>,
        requested_limit: Option<usize>,
    ) -> Result<LogPage, SupervisorError> {
        let limit = history_limit(requested_limit)?;
        let device = self.device(id).await?;
        let path = device_log_path(&device);
        let device_id = device.id.clone();
        tokio::task::spawn_blocking(move || read_log_page(&path, device_id, cursor, limit))
            .await
            .map_err(|error| {
                SupervisorError::Internal(anyhow::anyhow!("join daemon log read: {error}"))
            })?
            .map_err(SupervisorError::Internal)
    }

    pub async fn create_device(
        &self,
        id: String,
        label: String,
    ) -> Result<DeviceSnapshot, SupervisorError> {
        validate_device_id(&id)?;
        let label = label.trim().to_owned();
        validate_label(&label)?;
        let registration = RegisteredDevice {
            id: id.clone(),
            label,
        };
        let device = open_device(&self.inner.state_root, registration)?;
        {
            let mut devices = self.inner.devices.write().await;
            if devices.contains_key(&id) {
                return Err(SupervisorError::AlreadyExists(format!(
                    "device '{id}' already exists"
                )));
            }
            // Persisted before the map accepts it: a registry write that fails
            // must not leave a device this run can drive and the next cannot see.
            let mut pending = registrations(&devices);
            pending.push(device.registration());
            pending.sort_by(|left, right| left.id.cmp(&right.id));
            self.inner.registry.save(&pending)?;
            devices.insert(id.clone(), device.clone());
        }
        self.publish(EventKind::DeviceAdded, Some(id), "device added");
        let runtime = device.runtime.lock().await;
        Ok(snapshot_device(
            &device,
            &runtime,
            self.observation_of(&device.id),
            self.image(),
        ))
    }

    /// Change a registration in place. Renaming is safe at any lifecycle state:
    /// a label names the device to a person and nothing resolves by it.
    pub async fn update_device(
        &self,
        id: &str,
        request: UpdateDeviceRequest,
    ) -> Result<DeviceSnapshot, SupervisorError> {
        let device = self.device(id).await?;
        let Some(label) = request.label else {
            // Nothing asked for is not an error, and it is also not a write.
            let runtime = device.runtime.lock().await;
            return Ok(snapshot_device(
                &device,
                &runtime,
                self.observation_of(&device.id),
                self.image(),
            ));
        };
        let label = label.trim().to_owned();
        validate_label(&label)?;

        let devices = self.inner.devices.write().await;
        let previous = {
            let mut held = lock_recovering(&device.label);
            std::mem::replace(&mut *held, label)
        };
        if let Err(error) = self.inner.registry.save(&registrations(&devices)) {
            // Put the name back: the caller is about to be told this failed, and
            // an in-memory label the registry never accepted is a lie that
            // survives until the next restart.
            *lock_recovering(&device.label) = previous;
            return Err(error.into());
        }
        drop(devices);

        self.publish(
            EventKind::DeviceUpdated,
            Some(device.id.clone()),
            "device renamed",
        );
        let runtime = device.runtime.lock().await;
        Ok(snapshot_device(
            &device,
            &runtime,
            self.observation_of(&device.id),
            self.image(),
        ))
    }

    /// Forget a device, and — only when explicitly asked, and only when it is
    /// safe — destroy what it holds.
    ///
    /// Removal and deletion are separate operations wearing one call. Removal
    /// needs the device stopped and unowned; deletion additionally needs the
    /// home to canonicalize beneath the managed state root *now*, and the
    /// caller to name the device they are destroying. A path that does not
    /// canonicalize under the root is refused rather than deleted, because the
    /// alternative is a symlink deciding what this process erases.
    pub async fn remove_device(
        &self,
        id: &str,
        request: RemoveDeviceRequest,
    ) -> Result<(), SupervisorError> {
        let device = self.device(id).await?;
        if request.delete_data {
            match request.confirm.as_deref() {
                Some(confirmed) if confirmed == device.id => {}
                _ => {
                    return Err(SupervisorError::Invalid(format!(
                        "deleting device data requires confirm: \"{id}\""
                    )));
                }
            }
        }

        {
            let mut runtime = device.runtime.lock().await;
            self.refresh_owned_process(&device, &mut runtime);
            if runtime.process.is_some() {
                return Err(SupervisorError::Conflict(format!(
                    "device '{id}' is running; stop it before removing it"
                )));
            }
            match runtime.state {
                LifecycleState::Stopped | LifecycleState::Failed => {}
                LifecycleState::External => {
                    return Err(SupervisorError::Conflict(format!(
                        "device '{id}' has a daemon this supervisor does not own; \
                         stop it where it was started"
                    )));
                }
                state => {
                    return Err(SupervisorError::Conflict(format!(
                        "device '{id}' is {state:?} and cannot be removed"
                    )));
                }
            }
        }

        // Refused before anything is written, so a containment failure cannot
        // leave a forgotten registration behind pointing at surviving data.
        let doomed = if request.delete_data {
            Some(self.contained_home(&device)?)
        } else {
            None
        };

        {
            let mut devices = self.inner.devices.write().await;
            devices.remove(id);
            if let Err(error) = self.inner.registry.save(&registrations(&devices)) {
                devices.insert(id.to_owned(), device.clone());
                return Err(error.into());
            }
        }

        if let Some(home) = doomed {
            std::fs::remove_dir_all(&home).map_err(|error| {
                SupervisorError::Internal(anyhow::anyhow!(
                    "delete device home {}: {error}",
                    home.display()
                ))
            })?;
        }

        self.publish(
            EventKind::DeviceRemoved,
            Some(id.to_owned()),
            if request.delete_data {
                "device removed and its data deleted"
            } else {
                "device removed; its data was left in place"
            },
        );
        Ok(())
    }

    /// The device's home, proven right now to sit beneath the managed root.
    ///
    /// Containment is re-established at the moment of use rather than trusted
    /// from registration time: a directory can become a junction between the
    /// two, and the check is only worth anything if it observes what the
    /// delete will actually follow.
    fn contained_home(&self, device: &Device) -> Result<PathBuf, SupervisorError> {
        let home = std::fs::canonicalize(&device.home).map_err(|error| {
            SupervisorError::Invalid(format!(
                "device home {} cannot be resolved, so it will not be deleted: {error}",
                device.home.display()
            ))
        })?;
        let root = &self.inner.state_root;
        if !home.starts_with(root) || &home == root {
            return Err(SupervisorError::Invalid(format!(
                "device home {} is not contained by the managed root {}, so it will not be deleted",
                home.display(),
                root.display()
            )));
        }
        Ok(home)
    }

    // Keep this guard for the full transition so concurrent lifecycle requests cannot interleave.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn start_device(&self, id: &str) -> Result<DeviceSnapshot, SupervisorError> {
        let device = self.device(id).await?;
        let mut runtime = device.runtime.lock().await;
        self.refresh_owned_process(&device, &mut runtime);
        if runtime.process.is_some() {
            return Err(SupervisorError::Conflict(format!(
                "device '{id}' already has a managed process"
            )));
        }
        runtime.state = LifecycleState::Starting;
        runtime.last_error = None;
        self.publish(
            EventKind::LifecycleChanged,
            Some(id.to_owned()),
            "starting daemon",
        );

        match self.inner.driver.probe(&device.home).await {
            DaemonProbe::Healthy => {
                runtime.state = LifecycleState::External;
                let message =
                    format!("device '{id}' already has a daemon not owned by this workbench");
                runtime.last_error = Some(message.clone());
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(id.to_owned()),
                    "external daemon discovered",
                );
                return Err(SupervisorError::Conflict(message));
            }
            DaemonProbe::Foreign { why, replaceable } => {
                runtime.state = LifecycleState::Failed;
                let message = if replaceable {
                    format!("incompatible daemon detected; replace it explicitly: {why}")
                } else {
                    format!("incompatible daemon cannot be safely replaced: {why}")
                };
                runtime.last_error = Some(message.clone());
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(id.to_owned()),
                    "daemon compatibility check failed",
                );
                return Err(SupervisorError::Conflict(message));
            }
            DaemonProbe::Absent => {}
        }

        let process = match self.inner.driver.spawn(&device.home).await {
            Ok(process) => process,
            Err(error) => {
                runtime.state = LifecycleState::Failed;
                let message = format!("{error:#}");
                runtime.last_error = Some(message);
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(id.to_owned()),
                    "daemon spawn failed",
                );
                return Err(SupervisorError::Internal(error));
            }
        };
        runtime.started_at_ms = Some(now_ms());
        runtime.process = Some(process);

        let deadline = tokio::time::Instant::now()
            .checked_add(self.inner.start_timeout)
            .ok_or_else(|| {
                SupervisorError::Internal(anyhow::anyhow!("daemon startup deadline overflow"))
            })?;
        loop {
            if let Some(process) = runtime.process.as_mut() {
                match process.try_wait() {
                    Ok(Some(status)) => {
                        runtime.process = None;
                        runtime.state = LifecycleState::Failed;
                        let message = format!("daemon exited during startup ({status})");
                        runtime.last_error = Some(message.clone());
                        self.publish(
                            EventKind::LifecycleChanged,
                            Some(id.to_owned()),
                            "daemon exited during startup",
                        );
                        return Err(SupervisorError::Conflict(message));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        runtime.state = LifecycleState::Failed;
                        let message = format!("poll spawned daemon: {error}");
                        runtime.last_error = Some(message.clone());
                        return Err(SupervisorError::Internal(anyhow::anyhow!(message)));
                    }
                }
            }
            if matches!(
                self.inner.driver.probe(&device.home).await,
                DaemonProbe::Healthy
            ) {
                runtime.state = LifecycleState::Running;
                runtime.last_error = None;
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(id.to_owned()),
                    "daemon running",
                );
                return Ok(snapshot_device(
                    &device,
                    &runtime,
                    self.observation_of(&device.id),
                    self.image(),
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                if let Some(process) = runtime.process.as_mut() {
                    let _ = process.force_kill_and_wait();
                }
                runtime.process = None;
                runtime.state = LifecycleState::Failed;
                let message = format!(
                    "daemon did not become healthy within {:?}",
                    self.inner.start_timeout
                );
                runtime.last_error = Some(message.clone());
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(id.to_owned()),
                    "daemon startup timed out",
                );
                return Err(SupervisorError::Conflict(message));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    // Keep this guard for the full transition so concurrent lifecycle requests cannot interleave.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn stop_device(&self, id: &str) -> Result<DeviceSnapshot, SupervisorError> {
        let device = self.device(id).await?;
        let mut runtime = device.runtime.lock().await;
        self.refresh_owned_process(&device, &mut runtime);
        if runtime.process.is_none() {
            if matches!(
                self.inner.driver.probe(&device.home).await,
                DaemonProbe::Healthy
            ) {
                runtime.state = LifecycleState::External;
                return Err(SupervisorError::Conflict(format!(
                    "device '{id}' is running outside this workbench; it will not be stopped"
                )));
            }
            runtime.state = LifecycleState::Stopped;
            runtime.started_at_ms = None;
            runtime.last_error = None;
            return Ok(snapshot_device(
                &device,
                &runtime,
                self.observation_of(&device.id),
                self.image(),
            ));
        }
        runtime.state = LifecycleState::Stopping;
        self.publish(
            EventKind::LifecycleChanged,
            Some(id.to_owned()),
            "requesting graceful stop",
        );
        if let Err(error) = self.inner.driver.request_stop(&device.home).await {
            if !matches!(
                self.inner.driver.probe(&device.home).await,
                DaemonProbe::Absent
            ) {
                runtime.state = LifecycleState::Running;
                let message = format!("request graceful stop: {error:#}");
                runtime.last_error = Some(message.clone());
                return Err(SupervisorError::Conflict(message));
            }
        }

        let deadline = tokio::time::Instant::now()
            .checked_add(self.inner.stop_timeout)
            .ok_or_else(|| {
                SupervisorError::Internal(anyhow::anyhow!("daemon stop deadline overflow"))
            })?;
        loop {
            let exited = match runtime.process.as_mut() {
                Some(process) => match process.try_wait() {
                    Ok(status) => status.is_some(),
                    Err(error) => {
                        runtime.state = LifecycleState::Failed;
                        let message = format!("poll stopping daemon: {error}");
                        runtime.last_error = Some(message.clone());
                        return Err(SupervisorError::Internal(anyhow::anyhow!(message)));
                    }
                },
                None => true,
            };
            if exited {
                runtime.process = None;
                runtime.state = LifecycleState::Stopped;
                runtime.started_at_ms = None;
                runtime.last_error = None;
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(id.to_owned()),
                    "daemon stopped",
                );
                return Ok(snapshot_device(
                    &device,
                    &runtime,
                    self.observation_of(&device.id),
                    self.image(),
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                runtime.state = LifecycleState::Running;
                let message =
                    "graceful stop timed out; force_stop is available for this owned process"
                        .to_owned();
                runtime.last_error = Some(message.clone());
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(id.to_owned()),
                    "graceful stop timed out",
                );
                return Err(SupervisorError::Conflict(message));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    // Keep this guard for the full transition so concurrent lifecycle requests cannot interleave.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn force_stop_device(&self, id: &str) -> Result<DeviceSnapshot, SupervisorError> {
        let device = self.device(id).await?;
        let mut runtime = device.runtime.lock().await;
        self.refresh_owned_process(&device, &mut runtime);
        if runtime.process.is_none() {
            return Err(SupervisorError::Conflict(format!(
                "device '{id}' has no process owned by this workbench"
            )));
        }
        runtime.state = LifecycleState::Stopping;
        self.publish(
            EventKind::LifecycleChanged,
            Some(id.to_owned()),
            "force-stopping owned daemon",
        );
        let process = runtime.process.as_mut().ok_or_else(|| {
            SupervisorError::Conflict(format!(
                "device '{id}' has no process owned by this workbench"
            ))
        })?;
        process.force_kill_and_wait().map_err(|error| {
            SupervisorError::Internal(anyhow::anyhow!("force-stop daemon: {error}"))
        })?;
        runtime.process = None;
        runtime.state = LifecycleState::Stopped;
        runtime.started_at_ms = None;
        runtime.last_error = None;
        self.publish(
            EventKind::LifecycleChanged,
            Some(id.to_owned()),
            "daemon force-stopped",
        );
        Ok(snapshot_device(
            &device,
            &runtime,
            self.observation_of(&device.id),
            self.image(),
        ))
    }

    /// Stop and start one device, and tell every consumer to re-baseline.
    ///
    /// The lifecycle events alone describe the process coming down and going
    /// back up. They do not describe what the *new* process knows: a restarted
    /// daemon re-reads its store and re-dials its peers, so connection and fact
    /// state on the other side of a restart is not derivable from the events
    /// that crossed it. The `SnapshotRequired` goes out on the same stream, in
    /// sequence, which is what makes "everything before this is accounted for"
    /// a statement a consumer can act on.
    pub async fn restart_device(&self, id: &str) -> Result<DeviceSnapshot, SupervisorError> {
        self.stop_device(id).await?;
        let started = self.start_device(id).await?;
        self.publish_signal(ClientSignal::SnapshotRequired(
            SnapshotReason::DeviceRestarted {
                device_id: id.to_owned(),
            },
        ));
        Ok(started)
    }

    pub async fn snapshot(&self) -> WorkbenchSnapshot {
        self.observe().await;
        let devices: Vec<Arc<Device>> = self.inner.devices.read().await.values().cloned().collect();
        let mut snapshots = Vec::with_capacity(devices.len());
        for device in devices {
            let mut runtime = device.runtime.lock().await;
            self.refresh_owned_process(&device, &mut runtime);
            let snapshot = snapshot_device(
                &device,
                &runtime,
                self.observation_of(&device.id),
                self.image(),
            );
            drop(runtime);
            snapshots.push(snapshot);
        }
        let connections = lock_recovering(&self.inner.observed_connections)
            .values()
            .cloned()
            .collect();
        WorkbenchSnapshot {
            schema_version: SCHEMA_VERSION,
            revision: self.inner.revision.load(Ordering::SeqCst),
            environment: EnvironmentSnapshot {
                state_root: path_text(&self.inner.state_root),
                executable: path_text(&self.inner.executable),
                server_pid: std::process::id(),
            },
            capabilities: Capabilities::default(),
            devices: snapshots,
            connections,
            image: self.image(),
        }
    }

    /// Stop, rebuild, restage, restart — one operation.
    ///
    /// This is the whole reason staging exists. Today's remedy for "a running
    /// `lait.exe` holds its own image" is to terminate every daemon by hand,
    /// rebuild, restart them, and let every agent rediscover a tool surface that
    /// died underneath it. Reload does that in order, and `rebuild` never has to
    /// wait for a file to be released because nothing was holding the source.
    ///
    /// The build itself is the caller's: a supervisor library has no business
    /// knowing what a workspace is built with, and hard-coding one would be the
    /// UI-neutral core reaching for a development tool.
    ///
    /// Devices this supervisor does not own are stopped only if their identity
    /// can be proven, and are otherwise reported as left running. A reload that
    /// silently killed them would be the ownership boundary quietly lifted for
    /// convenience.
    pub async fn reload(
        &self,
        config: &Config,
        rebuild: impl std::future::Future<Output = Result<(), anyhow::Error>>,
    ) -> Result<Reload, SupervisorError> {
        let mut report = Reload::default();

        // Which devices were up, so the same set comes back.
        let devices: Vec<Arc<Device>> = self.inner.devices.read().await.values().cloned().collect();
        for device in &devices {
            let runtime = device.runtime.lock().await;
            match runtime.state {
                LifecycleState::Running if runtime.process.is_some() => {
                    report.was_running.push(device.id.clone());
                }
                LifecycleState::External => report.was_external.push(device.id.clone()),
                _ => {}
            }
        }

        for id in &report.was_running {
            if self.stop_device(id).await.is_err() {
                let _ = self.force_stop_device(id).await;
            }
            report.stopped.push(id.clone());
        }
        for id in &report.was_external {
            match self.stop_external(id).await {
                Ok(_) => report.stopped.push(id.clone()),
                Err(refusal) => report.left_running.push((id.clone(), refusal.to_string())),
            }
        }

        rebuild.await.map_err(SupervisorError::Internal)?;

        let image = StagedImage::prepare(&config.executable, &config.staging, now_ms())
            .map_err(SupervisorError::Internal)?;
        report.image = Some(image.facts().clone());
        *lock_recovering(&self.inner.image) = Some(image.facts().clone());
        self.inner.driver.restage(image.executable()).await;

        for id in &report.stopped {
            // A device left running was never stopped, so it is not restarted
            // either — it is still serving the image it came up with, which is
            // exactly what its `ImageFacts` will keep saying.
            if self.start_device(id).await.is_ok() {
                report.restarted.push(id.clone());
            }
        }

        self.publish_signal(ClientSignal::SnapshotRequired(SnapshotReason::Reloaded));
        Ok(report)
    }

    /// [`Supervisor::reload`] against the configuration this supervisor was
    /// started with, and with no build step of its own.
    ///
    /// The inner-loop case: the source was already rebuilt outside — a
    /// `cargo build` the person ran, a staging script, anything — and what is
    /// wanted is *restage and bring the same set back up on the new image*.
    /// The build stays the caller's exactly as it does for [`reload`]; this
    /// merely remembers which source and policy "the same" means.
    ///
    /// Refused for a supervisor constructed bare, where no staging config was
    /// ever stated — repeating a policy nobody set would be a guess.
    ///
    /// [`reload`]: Supervisor::reload
    pub async fn reload_in_place(&self) -> Result<Reload, SupervisorError> {
        let config = lock_recovering(&self.inner.reload_config)
            .clone()
            .ok_or_else(|| {
                SupervisorError::Conflict(
                    "this supervisor was not started from a configuration, so there is no \
                 staging policy to reload under"
                        .into(),
                )
            })?;
        self.reload(&config, async { Ok(()) }).await
    }

    /// Start a browser head against one device's daemon.
    ///
    /// Started from the *staged* image, like everything else this supervisor
    /// spawns, so a head is never the process holding the file a build is
    /// trying to replace. That is the tax this whole initiative exists to
    /// remove, and a head started from the workspace target would reintroduce
    /// it on the one process a person is most likely to leave running.
    pub async fn start_head(
        &self,
        device_id: &str,
        world: &str,
    ) -> Result<HeadFacts, SupervisorError> {
        let device = self.device(device_id).await?;
        let home = device.home.clone();
        self.spawn_head(
            format!("{device_id}-browser:{world}"),
            Some(device_id.to_owned()),
            Some(home),
            Some(world),
        )
        .await
    }

    /// Start a browser head against an identity home this supervisor does not
    /// manage — the person's own.
    ///
    /// Attach is the default relationship with that daemon: it is an
    /// always-running local service that outlives every window, and this does
    /// not change that. What it spawns is the *head*, which is ours and which
    /// nothing else on the machine is holding — and which is the only way `Open`
    /// has to reach a World, because the ticket it needs can only be minted by
    /// the process that will later be presented with it.
    ///
    /// Keyed by the home, so asking twice for the same identity finds the head
    /// that is already up instead of spending another port on a second one.
    pub async fn start_identity_head(
        &self,
        home: Option<&Path>,
        world: &str,
    ) -> Result<HeadFacts, SupervisorError> {
        let id = identity_head_id(home, world);
        // A head that is already up for this identity *is* the answer. Starting a
        // second would work and would still be wrong: two heads mean two run
        // credentials and two ports for one identity, and the second is the one
        // nothing later can find.
        //
        // **"Already up" is polled, not assumed, and that is a fix rather than a
        // refinement.** Exited heads now stay listed so a person can see that the
        // thing they opened died — and this lookup returned the entry
        // unconditionally, so the two composed into: a World whose head crashed
        // could never be opened again, because every `Open` handed back the dead
        // head's stale URL. Two correct changes, one broken composition, and a
        // symptom that would have read as "the browser opens on nothing".
        //
        // Three answers, because there are three states:
        let existing = {
            let mut heads = lock_recovering(&self.inner.heads);
            // Polled once, and the answer decides all three branches.
            let polled = heads
                .get_mut(&id)
                .map(|head| (head.refresh(), head.facts().clone()));
            match polled {
                None => None,
                // Alive. Reuse it, which is the whole point of the key.
                Some((crate::HeadState::Running, facts)) => Some(facts),
                // Gone. Take it out so the spawn below replaces it. Safe precisely
                // because it is gone: dropping the handle cannot orphan a process
                // `try_wait` has already reaped.
                Some((crate::HeadState::Exited { .. }, _)) => {
                    heads.remove(&id);
                    None
                }
                // Unknown: neither reuse nor replace. Reusing hands out a URL this
                // process cannot vouch for; replacing might start a second head
                // while the first is still serving. Saying so is the only honest
                // answer, and it is the arm that exists because the state has three
                // values rather than two.
                Some((crate::HeadState::Unknown { why }, _)) => {
                    return Err(SupervisorError::Internal(anyhow::anyhow!(
                        "head '{id}' could not be polled, so it is neither reused nor \
                         replaced: {why}"
                    )))
                }
            }
        };
        if let Some(existing) = existing {
            return Ok(existing);
        }
        self.spawn_head(id, None, home.map(Path::to_path_buf), Some(world))
            .await
    }

    async fn spawn_head(
        &self,
        id: String,
        device: Option<String>,
        home: Option<PathBuf>,
        world: Option<&str>,
    ) -> Result<HeadFacts, SupervisorError> {
        // Check and reserve together, under one lock, so a concurrent ask cannot
        // pass the check while this one is still starting a process.
        {
            let heads = lock_recovering(&self.inner.heads);
            let mut starting = lock_recovering(&self.inner.starting);
            if heads.contains_key(&id) {
                return Err(SupervisorError::AlreadyExists(format!(
                    "head '{id}' is already running"
                )));
            }
            if !starting.insert(id.clone()) {
                return Err(SupervisorError::AlreadyExists(format!(
                    "head '{id}' is already starting"
                )));
            }
        }
        // Released on every exit from here, including the error paths — a
        // reservation that outlived a failed spawn would make that World
        // permanently unstartable.
        let _reservation = Reservation {
            starting: &self.inner.starting,
            id: id.clone(),
        };

        let executable = self.inner.executable.clone();
        let facts_id = id.clone();
        let owner = device.clone();
        let world = world.map(str::to_owned);
        // Spawning and waiting for a readiness line are blocking, and both must
        // stay off whatever runtime thread asked: a head that takes its full
        // startup budget would otherwise stall every other task on that thread.
        let head = tokio::task::spawn_blocking(move || {
            start_browser(
                &executable,
                facts_id,
                owner,
                home.as_deref(),
                world.as_deref(),
            )
        })
        .await
        .map_err(|error| SupervisorError::Internal(anyhow::anyhow!("join head start: {error}")))?
        .map_err(SupervisorError::Internal)?;

        let facts = head.facts().clone();
        lock_recovering(&self.inner.heads).insert(id, head);
        self.publish(EventKind::LifecycleChanged, device, "browser head started");
        Ok(facts)
    }

    /// Every head this supervisor knows about.
    ///
    /// Owned browser heads only, for now: an MCP head is authored as a binding
    /// rather than started, and listing one would mean reading an agent
    /// harness's configuration — which is CLIENT-20's other half and belongs to
    /// whoever owns that file, not here.
    /// Every head this supervisor started, polled rather than remembered.
    ///
    /// **It polls.** That is the fix, not a detail: this used to clone facts
    /// recorded at spawn, so a head that had crashed read as running for the rest
    /// of the process's life and a person could not tell "I stopped it" from "it
    /// had already died". Devices were polled on every snapshot
    /// (`refresh_owned_process`); heads were the one lifecycle object here that
    /// was not.
    ///
    /// `&mut` is not needed by callers because the poll happens behind the same
    /// lock the list is read under — a head cannot be handed out as running and
    /// then found dead in the same breath.
    ///
    /// An exited head stays in the list. Removing it would make a row that died
    /// indistinguishable from one that was never started, which is the same
    /// absence-versus-verdict confusion in a different costume.
    pub fn list_heads(&self) -> Vec<HeadFacts> {
        let mut heads = lock_recovering(&self.inner.heads);
        heads
            .values_mut()
            .map(|head| {
                head.refresh();
                head.facts().clone()
            })
            .collect()
    }

    /// Stop a head this supervisor started.
    ///
    /// The handle is the proof, exactly as it is for a daemon. There is no
    /// pid-based path here at all, which is what makes "no external process can
    /// be stopped" true of heads by construction rather than by check.
    /// Returns *which* success it was: stopped, or already gone. A caller that
    /// discards the distinction is choosing to; one that never had it could not.
    pub async fn stop_head(&self, id: &str) -> Result<crate::heads::Stopped, SupervisorError> {
        let head = lock_recovering(&self.inner.heads).remove(id);
        let head = head.ok_or_else(|| {
            SupervisorError::NotFound(format!(
                "no head '{id}' was started by this supervisor, so it cannot be stopped here"
            ))
        })?;
        let device = head.facts().device.clone();
        let outcome = tokio::task::spawn_blocking(move || head.stop())
            .await
            .map_err(|error| SupervisorError::Internal(anyhow::anyhow!("join head stop: {error}")))?
            .map_err(SupervisorError::Internal)?;
        // The two successes are published differently, because the second one is
        // news. A person who presses stop on a World that had already crashed
        // should learn that it crashed, not be told their button worked.
        self.publish(
            EventKind::LifecycleChanged,
            device,
            match &outcome {
                crate::heads::Stopped::Stopped => "browser head stopped",
                crate::heads::Stopped::Forced => {
                    "browser head did not shut down in time and was forced"
                }
                crate::heads::Stopped::WasAlreadyGone { .. } => {
                    "browser head had already exited before it was stopped"
                }
            },
        );
        Ok(outcome)
    }

    /// The image devices are spawned from, once one has been staged.
    pub fn image(&self) -> Option<ImageFacts> {
        lock_recovering(&self.inner.image).clone()
    }

    /// Stop a daemon this supervisor did not spawn, on evidence.
    ///
    /// Ownership is still the safety boundary and this does not weaken it: an
    /// external daemon is stopped only when its identity can be *proven* to be
    /// the one recorded, and an unprovable identity is a refusal. The proof is
    /// four facts agreeing — the pid the home records, the executable that pid
    /// is running, when it started, and that the home is one this supervisor
    /// manages — and the last two are what a stale pid file cannot forge.
    ///
    /// The verification happens inside `terminate_verified`, on the handle that
    /// does the terminating, so nothing can be raced between checking and
    /// killing.
    pub async fn stop_external(&self, id: &str) -> Result<DeviceSnapshot, SupervisorError> {
        let device = self.device(id).await?;
        {
            let runtime = device.runtime.lock().await;
            if runtime.process.is_some() {
                return Err(SupervisorError::Conflict(format!(
                    "device '{id}' is owned by this supervisor; stop it normally"
                )));
            }
            if runtime.state != LifecycleState::External {
                return Err(SupervisorError::Conflict(format!(
                    "device '{id}' has no external daemon to stop"
                )));
            }
        }

        let recorded = self.observation_of(id).identity.ok_or_else(|| {
            SupervisorError::Conflict(format!(
                "device '{id}' has no proven process identity, so nothing may be stopped"
            ))
        })?;
        self.inner
            .driver
            .terminate_verified(&recorded)
            .await
            .map_err(|error| SupervisorError::Conflict(format!("{error:#}")))?;

        self.publish(
            EventKind::LifecycleChanged,
            Some(id.to_owned()),
            "external daemon stopped on verified identity",
        );
        self.observe().await;
        let runtime = device.runtime.lock().await;
        Ok(snapshot_device(
            &device,
            &runtime,
            self.observation_of(id),
            self.image(),
        ))
    }

    /// Stop every daemon this supervisor owns, leaving externally discovered
    /// daemons untouched. Intended for graceful server shutdown and tests.
    pub async fn stop_all_owned(&self) {
        let ids: Vec<String> = self.inner.devices.read().await.keys().cloned().collect();
        for id in ids {
            if self.stop_device(&id).await.is_err() {
                let _ = self.force_stop_device(&id).await;
            }
        }
    }

    async fn device(&self, id: &str) -> Result<Arc<Device>, SupervisorError> {
        self.inner
            .devices
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| SupervisorError::NotFound(format!("device '{id}' does not exist")))
    }

    fn refresh_owned_process(&self, device: &Device, runtime: &mut Runtime) {
        let status = match runtime.process.as_mut() {
            Some(process) => process.try_wait(),
            None => return,
        };
        match status {
            Ok(Some(exit)) => {
                runtime.process = None;
                runtime.state = LifecycleState::Failed;
                runtime.started_at_ms = None;
                runtime.last_error = Some(format!("daemon exited unexpectedly ({exit})"));
                self.publish(
                    EventKind::LifecycleChanged,
                    Some(device.id.clone()),
                    "daemon exited unexpectedly",
                );
            }
            Ok(None) => {}
            Err(error) => {
                runtime.last_error = Some(format!("poll daemon process: {error}"));
            }
        }
    }

    /// Fold this pass's peers into the observed topology.
    ///
    /// `sampled` names the devices this pass actually read. Only those devices'
    /// rows may be retired: a peer belonging to a device nobody could ask is
    /// *kept*, because the alternative is publishing a `Disconnected` for a
    /// connection that was never observed to end. That is the defect this
    /// signature exists to make impossible — a sampling failure must not be
    /// reportable as a disconnection.
    fn record_connection_observations(
        &self,
        current: BTreeMap<ConnectionKey, ConnectionSnapshot>,
        sampled: &[String],
    ) {
        let changes = {
            let mut previous = lock_recovering(&self.inner.observed_connections);
            let mut changes = Vec::new();
            for (key, connection) in &current {
                match previous.get(key) {
                    None => changes.push((ConnectionEventKind::Connected, connection.clone())),
                    Some(old) if old != connection => {
                        changes.push((ConnectionEventKind::Changed, connection.clone()));
                    }
                    Some(_) => {}
                }
            }
            let mut merged = current;
            for (key, connection) in previous.iter() {
                if merged.contains_key(key) {
                    continue;
                }
                if sampled.contains(&connection.source_device_id) {
                    changes.push((ConnectionEventKind::Disconnected, connection.clone()));
                } else {
                    merged.insert(key.clone(), connection.clone());
                }
            }
            *previous = merged;
            changes
        };
        for (kind, connection) in changes {
            self.publish_connection(kind, connection);
        }
    }

    fn record_log_observations(&self, current: BTreeMap<String, u64>) {
        let changes = {
            let mut previous = lock_recovering(&self.inner.observed_log_sizes);
            let changes = current
                .iter()
                .filter_map(|(device_id, size)| {
                    previous.get(device_id).and_then(|old_size| {
                        (*old_size != *size).then(|| (device_id.clone(), *old_size, *size))
                    })
                })
                .collect::<Vec<_>>();
            *previous = current;
            changes
        };
        for (device_id, old_size, new_size) in changes {
            let message = if new_size < old_size {
                "daemon log reset"
            } else {
                "daemon log appended"
            };
            self.publish(EventKind::LogChanged, Some(device_id), message);
        }
    }

    fn publish_connection(&self, kind: ConnectionEventKind, connection: ConnectionSnapshot) {
        let at_ms = now_ms();
        let backend_event = {
            let mut history = lock_recovering(&self.inner.history);
            let revision = self.next_revision();
            if let Some(dropped) = push_bounded(
                &mut history.connections,
                ConnectionEvent {
                    revision,
                    at_ms,
                    kind,
                    connection: connection.clone(),
                },
                CONNECTION_HISTORY_CAPACITY,
            ) {
                history.dropped_connections_through = Some(dropped.revision);
            }
            let event = BackendEvent {
                revision,
                at_ms,
                kind: EventKind::ConnectionChanged,
                device_id: Some(connection.source_device_id),
                message: match kind {
                    ConnectionEventKind::Connected => "connection observed",
                    ConnectionEventKind::Changed => "connection state changed",
                    ConnectionEventKind::Disconnected => "connection no longer observed",
                }
                .to_owned(),
            };
            if let Some(dropped) =
                push_bounded(&mut history.events, event.clone(), EVENT_HISTORY_CAPACITY)
            {
                history.dropped_events_through = Some(dropped.revision);
            }
            event
        };
        let _ = self.inner.signals.send(ClientSignal::Event(backend_event));
    }

    fn publish(&self, kind: EventKind, device_id: Option<String>, message: impl Into<String>) {
        let event = {
            let mut history = lock_recovering(&self.inner.history);
            let event = BackendEvent {
                revision: self.next_revision(),
                at_ms: now_ms(),
                kind,
                device_id,
                message: message.into(),
            };
            if let Some(dropped) =
                push_bounded(&mut history.events, event.clone(), EVENT_HISTORY_CAPACITY)
            {
                history.dropped_events_through = Some(dropped.revision);
            }
            event
        };
        self.publish_signal(ClientSignal::Event(event));
    }

    /// Put one signal on the stream.
    ///
    /// A send with no receivers is not a failure — nobody is listening yet, or
    /// the last consumer went away, and neither is the supervisor's problem.
    fn publish_signal(&self, signal: ClientSignal) {
        let _ = self.inner.signals.send(signal);
    }

    fn next_revision(&self) -> u64 {
        self.inner
            .revision
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }
}

fn snapshot_device(
    device: &Device,
    runtime: &Runtime,
    observation: DeviceObservation,
    image: Option<ImageFacts>,
) -> DeviceSnapshot {
    DeviceSnapshot {
        id: device.id.clone(),
        label: device.label(),
        home: path_text(&device.home),
        log_path: path_text(&device_log_path(device)),
        state: runtime.state,
        pid: runtime.process.as_ref().map(|process| process.id()),
        owned: runtime.process.is_some(),
        started_at_ms: runtime.started_at_ms,
        last_error: runtime.last_error.clone(),
        facts: observation.facts,
        observation: observation.health,
        image,
    }
}

/// A log that cannot be stat'd reads as zero rather than as an error: the log
/// is a growth signal, not a fact anything depends on, and a device that has
/// never written one is the ordinary case.
fn log_size(device: &Device) -> u64 {
    std::fs::metadata(device_log_path(device))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn device_log_path(device: &Device) -> PathBuf {
    let daemon_home = lait::config::Selection::for_identity(&device.home)
        .daemon_home()
        .unwrap_or_else(|_| device.home.join("daemon"));
    lait::host_client::daemon_log_path(&daemon_home)
}

fn connection_key(connection: &ConnectionSnapshot) -> ConnectionKey {
    (
        connection.source_device_id.clone(),
        connection.space_id.clone(),
        connection.peer_id.clone(),
    )
}

fn history_limit(requested: Option<usize>) -> Result<usize, SupervisorError> {
    let limit = requested.unwrap_or(DEFAULT_PAGE_LIMIT);
    if limit == 0 || limit > MAX_PAGE_LIMIT {
        return Err(SupervisorError::Invalid(format!(
            "limit must be between 1 and {MAX_PAGE_LIMIT}"
        )));
    }
    Ok(limit)
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) -> Option<T> {
    let dropped = if queue.len() == capacity {
        queue.pop_front()
    } else {
        None
    };
    queue.push_back(value);
    dropped
}

fn lock_recovering<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn open_device(
    state_root: &Path,
    registration: RegisteredDevice,
) -> Result<Arc<Device>, SupervisorError> {
    let home = state_root.join("devices").join(&registration.id);
    std::fs::create_dir_all(&home).map_err(|error| {
        SupervisorError::Internal(anyhow::anyhow!(
            "create device home {}: {error}",
            home.display()
        ))
    })?;
    let home = std::fs::canonicalize(&home).map_err(|error| {
        SupervisorError::Internal(anyhow::anyhow!(
            "canonicalize device home {}: {error}",
            home.display()
        ))
    })?;
    if !home.starts_with(state_root) {
        return Err(SupervisorError::Invalid(
            "device home escaped the workbench state root".into(),
        ));
    }
    Ok(Arc::new(Device {
        id: registration.id,
        label: StdMutex::new(registration.label),
        home,
        runtime: Mutex::new(Runtime {
            state: LifecycleState::Stopped,
            process: None,
            started_at_ms: None,
            last_error: None,
        }),
    }))
}

fn validate_device_id(id: &str) -> Result<(), SupervisorError> {
    if id.is_empty() || id.len() > 48 {
        return Err(SupervisorError::Invalid(
            "device id must contain 1 to 48 characters".into(),
        ));
    }
    let valid = id.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || (byte == b'-' && index > 0)
    });
    if !valid || id.ends_with('-') {
        return Err(SupervisorError::Invalid(
            "device id must use lowercase letters, digits, and interior hyphens".into(),
        ));
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<(), SupervisorError> {
    if label.trim().is_empty() || label.trim() != label || label.len() > 80 {
        return Err(SupervisorError::Invalid(
            "label must contain 1 to 80 characters".into(),
        ));
    }
    Ok(())
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The id a head for one identity is filed under.
///
/// The home *is* the key, and its absence is a key too — the ordinary per-user
/// identity is one identity like any other and gets one head like any other. A
/// generated id would let an identity accumulate heads nobody can find again,
/// and a counter would make "is there already a head for this identity" a scan
/// of every value instead of a lookup.
/// One head per identity *and World*.
///
/// The World is in the key because it is in the head: two Worlds are two
/// processes, so a key that named only the identity would find the first and
/// hand it back for the second — which is the shared-head behaviour the pin
/// exists to end, reintroduced one layer up.
///
/// **The mount is not optional, and that is the whole guarantee.** It used to be
/// `Option<&str>` spelled into the key as `world.unwrap_or("default")`, while the
/// facts recorded what the head *announced*. So one caller asking for `None` and
/// another asking for `Some("issues")` built two different keys for one World:
/// two heads, two ports, two run credentials, both listed, both matching the
/// row — and `stop` reached one of them while the row went on saying Running.
/// Resolving "which World did they mean" belongs to whoever knows the build, and
/// a key cannot be built here without an answer.
fn identity_head_id(home: Option<&Path>, world: &str) -> String {
    match home {
        Some(home) => format!("identity:{}:{world}", path_text(home)),
        None => format!("identity:default:{world}"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use async_trait::async_trait;

    struct FakeDriver {
        alive: Arc<AtomicBool>,
        connections: Arc<StdMutex<Vec<crate::driver::ObservedConnection>>>,
        /// When set, sampling fails — the daemon is up but cannot be asked,
        /// which is the case the whole observation-health contract exists for.
        sampling_fails: Arc<AtomicBool>,
        station_id: Arc<StdMutex<Option<String>>>,
        identity: Arc<StdMutex<Option<lait::daemon_spawn::ProcessIdentity>>>,
        /// When set, terminating refuses — the process is no longer the one
        /// that was recorded.
        terminate_refusal: Arc<StdMutex<Option<String>>>,
    }

    struct FakeChild {
        alive: Arc<AtomicBool>,
    }

    impl OwnedDaemon for FakeChild {
        fn id(&self) -> u32 {
            42
        }

        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            if self.alive.load(Ordering::SeqCst) {
                Ok(None)
            } else {
                Ok(Some(success_status()))
            }
        }

        fn force_kill_and_wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            self.alive.store(false, Ordering::SeqCst);
            Ok(success_status())
        }
    }

    #[async_trait]
    impl DaemonDriver for FakeDriver {
        async fn probe(&self, _home: &Path) -> DaemonProbe {
            if self.alive.load(Ordering::SeqCst) {
                DaemonProbe::Healthy
            } else {
                DaemonProbe::Absent
            }
        }

        async fn spawn(&self, _home: &Path) -> anyhow::Result<Box<dyn OwnedDaemon>> {
            self.alive.store(true, Ordering::SeqCst);
            Ok(Box::new(FakeChild {
                alive: self.alive.clone(),
            }))
        }

        async fn request_stop(&self, _home: &Path) -> anyhow::Result<()> {
            self.alive.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn identity(
            &self,
            _home: &Path,
        ) -> anyhow::Result<lait::daemon_spawn::ProcessIdentity> {
            lock_recovering(&self.identity)
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no pid recorded for this home"))
        }

        async fn terminate_verified(
            &self,
            _expected: &lait::daemon_spawn::ProcessIdentity,
        ) -> anyhow::Result<()> {
            if let Some(refusal) = lock_recovering(&self.terminate_refusal).clone() {
                return Err(anyhow::anyhow!(refusal));
            }
            self.alive.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn facts(&self, _home: &Path) -> anyhow::Result<DeviceFacts> {
            if self.sampling_fails.load(Ordering::SeqCst) {
                return Err(anyhow::anyhow!("control channel refused"));
            }
            Ok(DeviceFacts {
                version: Some("0.0.0-test".into()),
                station_id: lock_recovering(&self.station_id).clone(),
                ..DeviceFacts::default()
            })
        }

        async fn connections(
            &self,
            _home: &Path,
        ) -> anyhow::Result<Vec<crate::driver::ObservedConnection>> {
            if self.sampling_fails.load(Ordering::SeqCst) {
                return Err(anyhow::anyhow!("control channel refused"));
            }
            Ok(lock_recovering(&self.connections).clone())
        }
    }

    /// The knobs a test reaches for, so a test that only cares about one of
    /// them does not have to name the others.
    #[derive(Clone, Default)]
    struct Fake {
        connections: Arc<StdMutex<Vec<crate::driver::ObservedConnection>>>,
        sampling_fails: Arc<AtomicBool>,
        station_id: Arc<StdMutex<Option<String>>>,
        identity: Arc<StdMutex<Option<lait::daemon_spawn::ProcessIdentity>>>,
        terminate_refusal: Arc<StdMutex<Option<String>>>,
    }

    fn fake_supervisor(alive: Arc<AtomicBool>, root: &Path) -> Supervisor {
        fake_supervisor_with(alive, &Fake::default(), root)
    }

    fn fake_supervisor_with_connections(
        alive: Arc<AtomicBool>,
        connections: Arc<StdMutex<Vec<crate::driver::ObservedConnection>>>,
        root: &Path,
    ) -> Supervisor {
        fake_supervisor_with(
            alive,
            &Fake {
                connections,
                ..Fake::default()
            },
            root,
        )
    }

    fn fake_supervisor_with(alive: Arc<AtomicBool>, fake: &Fake, root: &Path) -> Supervisor {
        let root = std::fs::canonicalize(root).expect("canonical test root");
        Supervisor::with_driver(
            root,
            PathBuf::from("fake-lait"),
            Arc::new(FakeDriver {
                alive,
                connections: fake.connections.clone(),
                sampling_fails: fake.sampling_fails.clone(),
                station_id: fake.station_id.clone(),
                identity: fake.identity.clone(),
                terminate_refusal: fake.terminate_refusal.clone(),
            }),
            Duration::from_millis(100),
            Duration::from_millis(100),
        )
        .expect("fake supervisor")
    }

    #[test]
    fn device_ids_cannot_escape_the_managed_root() {
        for id in ["../alice", "Alice", "alice/bob", "-alice", "alice-", ""] {
            assert!(validate_device_id(id).is_err(), "{id:?}");
        }
        for id in ["alice", "alice-2", "d3"] {
            assert!(validate_device_id(id).is_ok(), "{id:?}");
        }
    }

    #[tokio::test]
    async fn owns_a_device_through_start_and_graceful_stop() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive.clone(), directory.path());

        let added = supervisor
            .create_device("alice".into(), "Alice's laptop".into())
            .await
            .expect("add device");
        assert_eq!(added.state, LifecycleState::Stopped);

        let running = supervisor
            .start_device("alice")
            .await
            .expect("start device");
        assert_eq!(running.state, LifecycleState::Running);
        assert_eq!(running.pid, Some(42));
        assert!(running.owned);

        let stopped = supervisor.stop_device("alice").await.expect("stop device");
        assert_eq!(stopped.state, LifecycleState::Stopped);
        assert!(!stopped.owned);
        assert!(!alive.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn shutdown_stops_observing_and_stops_what_it_owns() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive.clone(), directory.path());
        supervisor.observe_in_background(Duration::from_millis(5));
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("add device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");

        supervisor.shutdown().await;

        assert!(!alive.load(Ordering::SeqCst), "owned daemon still running");
        assert!(
            lock_recovering(&supervisor.inner.observer).is_none(),
            "sampler survived shutdown"
        );
    }

    /// The sampler holds a weak reference, so it cannot keep its own supervisor
    /// alive. A strong one would be a cycle: the task would own the `Inner` that
    /// owns the task's join handle, nothing would ever drop, and the sampling
    /// would outlive every handle to it for the life of the process.
    #[tokio::test]
    async fn background_observation_does_not_outlive_its_supervisor() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());
        supervisor.observe_in_background(Duration::from_millis(5));
        let observed = Arc::downgrade(&supervisor.inner);

        drop(supervisor);

        assert!(
            observed.upgrade().is_none(),
            "the sampler is holding its supervisor alive"
        );
    }

    fn identity(pid: u32, exe: &str, started_at_ms: u64) -> lait::daemon_spawn::ProcessIdentity {
        lait::daemon_spawn::ProcessIdentity {
            pid,
            executable: PathBuf::from(exe),
            started_at_ms,
        }
    }

    /// Ownership stays the safety boundary. An external daemon whose identity
    /// cannot be proven is refused, and refusing is the *default* — the proof
    /// has to arrive, not the doubt.
    #[tokio::test]
    async fn an_unproven_external_daemon_is_never_stopped() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(true));
        let fake = Fake::default();
        let supervisor = fake_supervisor_with(alive.clone(), &fake, directory.path());
        supervisor
            .create_device("bob".into(), "Bob".into())
            .await
            .expect("create device");
        supervisor.observe().await;
        assert_eq!(
            supervisor.snapshot().await.devices[0].state,
            LifecycleState::External
        );

        // No identity was provable, so there is nothing to stop on.
        assert!(matches!(
            supervisor.stop_external("bob").await,
            Err(SupervisorError::Conflict(_))
        ));
        assert!(
            alive.load(Ordering::SeqCst),
            "an unproven daemon was killed"
        );

        // With an identity recorded, the driver is asked — and this one refuses,
        // standing in for the process having changed underneath the record.
        *lock_recovering(&fake.identity) = Some(identity(4242, "lait.exe", 1_000));
        *lock_recovering(&fake.terminate_refusal) =
            Some("process 4242 is not the one recorded".into());
        supervisor.observe().await;
        assert!(matches!(
            supervisor.stop_external("bob").await,
            Err(SupervisorError::Conflict(_))
        ));
        assert!(
            alive.load(Ordering::SeqCst),
            "a daemon whose identity no longer matched was killed anyway"
        );

        // Only when the evidence holds does it stop.
        *lock_recovering(&fake.terminate_refusal) = None;
        supervisor
            .stop_external("bob")
            .await
            .expect("stop external");
        assert!(!alive.load(Ordering::SeqCst));
    }

    /// There is no pid-based path to stopping a head at all, which is what
    /// makes "no external process can be stopped" true of heads by
    /// construction. A head this supervisor did not start is simply not
    /// something it can be asked about.
    #[tokio::test]
    async fn a_head_this_supervisor_did_not_start_cannot_be_stopped_through_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());

        assert!(supervisor.list_heads().is_empty());
        assert!(matches!(
            supervisor.stop_head("somebody-elses-head").await,
            Err(SupervisorError::NotFound(_))
        ));
    }

    /// A World whose head died can be opened again.
    ///
    /// The regression this exists for was a composition, not a mistake in either
    /// half. Exited heads stay listed so a person can see that the thing they
    /// opened died; `start_identity_head` reused whatever was under the key. Put
    /// together: every `Open` on a crashed World handed back the dead head's stale
    /// URL, forever, and the symptom would have read as "the browser opens on
    /// nothing".
    ///
    /// Driven through the map directly rather than by spawning a real `lait`: the
    /// property under test is what the lookup does with a dead entry, and a test
    /// that needed the binary would be testing the launcher instead.
    #[tokio::test]
    async fn a_world_whose_head_died_is_opened_again_rather_than_handed_a_dead_url() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());

        let id = identity_head_id(Some(directory.path()), "issues");
        lock_recovering(&supervisor.inner.heads)
            .insert(id.clone(), crate::heads::dead_head_for_test(id.clone()));

        // It reports as exited rather than running, which is the first half.
        let listed = supervisor.list_heads();
        assert_eq!(listed.len(), 1, "a dead head stays listed");
        assert!(
            matches!(listed[0].state, crate::HeadState::Exited { .. }),
            "a dead head reports exited, not running: {:?}",
            listed[0].state
        );

        // And the second half: asking for that World does not hand the dead entry
        // back. It has no `lait` to spawn here, so the *shape* of the answer is what
        // matters — anything but `Ok(dead facts)` proves the entry was not reused.
        let asked = supervisor
            .start_identity_head(Some(directory.path()), "issues")
            .await;
        match asked {
            Ok(facts) => panic!(
                "a dead head was handed back as the answer: {:?} / {:?}",
                facts.state, facts.url
            ),
            Err(error) => {
                let said = format!("{error}");
                assert!(
                    !said.contains("could not be polled"),
                    "a dead head must be replaced, not reported unpollable: {said}"
                );
            }
        }
        assert!(
            !lock_recovering(&supervisor.inner.heads).contains_key(&id),
            "the dead entry must be taken out of the map so a spawn can replace it"
        );
    }

    /// Two concurrent asks for one World start one head, not two.
    ///
    /// The window this closes was 20 seconds wide and invisible to every other test:
    /// `spawn_head` checked the map, released the lock while a real process started
    /// and announced itself, then inserted. Two asks both passed the check, both
    /// started a `lait`, and the second insert dropped the first `OwnedHead` —
    /// which neither kills nor waits — leaving an orphan holding a port and a live
    /// run credential, unlistable and unstoppable.
    ///
    /// Keying by mount was claimed to have fixed this and did not: the key was never
    /// the hole. Concurrency was.
    ///
    /// Asserted through the reservation rather than by spawning two real heads: the
    /// property is mutual exclusion over one key, and a test that raced two binaries
    /// would be slow and would only *usually* interleave the way that matters.
    #[tokio::test]
    async fn two_concurrent_asks_for_one_world_do_not_both_start_a_head() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());
        let id = identity_head_id(Some(directory.path()), "issues");

        // Stand in for a spawn already in flight for this key, which is exactly the
        // state the old code could not represent.
        lock_recovering(&supervisor.inner.starting).insert(id.clone());

        let second = supervisor
            .start_identity_head(Some(directory.path()), "issues")
            .await;
        match second {
            Err(SupervisorError::AlreadyExists(said)) => assert!(
                said.contains("already starting"),
                "the second ask must be told a spawn is in flight, not handed a \
                 second head: {said}"
            ),
            other => panic!(
                "a second concurrent ask was not excluded: {:?}",
                other.map(|facts| facts.id)
            ),
        }

        // And the reservation is given back, so a failed or finished spawn does not
        // make the World permanently unstartable.
        lock_recovering(&supervisor.inner.starting).remove(&id);
        assert!(
            !lock_recovering(&supervisor.inner.starting).contains(&id),
            "a reservation must not outlive its spawn"
        );
    }

    /// An owned device is never routed through the unowned path: a handle is
    /// stronger evidence than anything that can be read back about a pid.
    #[tokio::test]
    async fn an_owned_device_is_refused_by_the_unowned_stop() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let fake = Fake::default();
        *lock_recovering(&fake.identity) = Some(identity(4242, "lait.exe", 1_000));
        let supervisor = fake_supervisor_with(alive.clone(), &fake, directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");

        assert!(matches!(
            supervisor.stop_external("alice").await,
            Err(SupervisorError::Conflict(_))
        ));
        assert!(alive.load(Ordering::SeqCst));
    }

    /// Reload stops what it owns, rebuilds, restages, and brings the same set
    /// back — and reports what it could not stop rather than stopping it.
    #[tokio::test]
    async fn reload_restarts_what_it_owned_and_reports_what_it_left() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("lait.exe");
        std::fs::write(&source, b"build one").expect("write executable");
        let config =
            Config::new(source.clone(), source.clone()).staged_in(directory.path().join("staging"));

        let alive = Arc::new(AtomicBool::new(false));
        let fake = Fake::default();
        let supervisor = fake_supervisor_with(alive, &fake, directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");

        let rebuilt = Arc::new(AtomicBool::new(false));
        let flag = rebuilt.clone();
        let report = supervisor
            .reload(&config, async move {
                // The build runs with nothing holding the source, which is the
                // whole point of staging.
                std::fs::write(&source, b"build two")?;
                flag.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .expect("reload");

        assert!(rebuilt.load(Ordering::SeqCst), "the rebuild never ran");
        assert_eq!(report.was_running, vec!["alice".to_owned()]);
        assert_eq!(report.stopped, vec!["alice".to_owned()]);
        assert_eq!(report.restarted, vec!["alice".to_owned()]);
        assert!(report.left_running.is_empty());

        let image = report.image.expect("reload staged an image");
        assert_ne!(
            image.source_path, image.staged_path,
            "a staged reload ran from the source anyway"
        );
        assert_eq!(
            std::fs::read(&image.staged_path).expect("read staged"),
            b"build two",
            "the restaged image is not the rebuilt one"
        );
        assert_eq!(
            supervisor.snapshot().await.devices[0]
                .image
                .as_ref()
                .map(|facts| facts.fingerprint.clone()),
            Some(image.fingerprint),
            "the device does not report the image it is actually running"
        );
    }

    /// An in-place reload repeats the staging the supervisor was started
    /// with: the source was rebuilt outside, and the same set comes back on
    /// the new image. A bare supervisor — no stated staging — is refused
    /// rather than guessed at.
    #[tokio::test]
    async fn an_in_place_reload_restages_from_the_remembered_configuration() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("lait.exe");
        std::fs::write(&source, b"build one").expect("write executable");
        let config =
            Config::new(source.clone(), source.clone()).staged_in(directory.path().join("staging"));

        let alive = Arc::new(AtomicBool::new(false));
        let fake = Fake::default();
        let supervisor = fake_supervisor_with(alive, &fake, directory.path());

        // Bare: nothing was staged and no policy was stated.
        assert!(matches!(
            supervisor.reload_in_place().await,
            Err(SupervisorError::Conflict(_))
        ));

        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");

        *lock_recovering(&supervisor.inner.reload_config) = Some(config);
        std::fs::write(&source, b"build two").expect("rebuild outside");

        let report = supervisor.reload_in_place().await.expect("reload in place");
        assert_eq!(report.restarted, vec!["alice".to_owned()]);
        let image = report.image.expect("an in-place reload staged an image");
        assert_eq!(
            std::fs::read(&image.staged_path).expect("read staged"),
            b"build two",
            "the in-place reload did not pick up the outside rebuild"
        );
        assert_eq!(
            supervisor
                .snapshot()
                .await
                .image
                .map(|facts| facts.fingerprint),
            Some(image.fingerprint),
            "the snapshot does not carry the image the bench would spawn today"
        );
    }

    /// Everything currently readable from a stream without blocking, so a test
    /// can assert on a whole sequence rather than one signal at a time.
    async fn drain(signals: &mut Signals) -> Vec<ClientSignal> {
        let mut drained = Vec::new();
        while let Ok(Some(signal)) =
            tokio::time::timeout(Duration::from_millis(50), signals.recv()).await
        {
            drained.push(signal);
        }
        drained
    }

    fn revisions(signals: &[ClientSignal]) -> Vec<u64> {
        signals
            .iter()
            .filter_map(|signal| match signal {
                ClientSignal::Event(event) => Some(event.revision),
                _ => None,
            })
            .collect()
    }

    /// The stream must exist before the first observation, or the window
    /// between them drops events with nothing to say so. `start` returning both
    /// together is what makes that unmissable.
    #[tokio::test]
    async fn the_stream_is_established_before_the_first_snapshot() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(directory.path()).expect("canonical root");
        // A real file: `start` stages the image, and staging reads it.
        let executable = root.join("lait.exe");
        std::fs::write(&executable, b"build one").expect("write executable");
        let (supervisor, mut signals) = Supervisor::start(Config {
            state_root: root,
            executable,
            observation_interval: Duration::from_secs(3_600),
            staging: Staging::Direct,
        })
        .await
        .expect("start");

        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");

        let drained = drain(&mut signals).await;
        assert!(
            matches!(
                drained.first(),
                Some(ClientSignal::Event(event)) if matches!(event.kind, EventKind::DeviceAdded)
            ),
            "the first signal after start was not the first thing that happened: {drained:?}"
        );
        supervisor.shutdown().await;
    }

    /// Revisions are monotonic across a supervised process restart, and the
    /// re-baseline point is *on the same stream* rather than beside it.
    #[tokio::test]
    async fn signals_stay_ordered_across_a_supervised_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());
        let mut signals = supervisor.signals();
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");
        supervisor
            .restart_device("alice")
            .await
            .expect("restart device");

        let drained = drain(&mut signals).await;
        let seen = revisions(&drained);
        assert!(
            seen.windows(2).all(|pair| pair[0] < pair[1]),
            "revisions went backwards across a restart: {seen:?}"
        );

        let restart_at = drained
            .iter()
            .position(|signal| {
                matches!(
                    signal,
                    ClientSignal::SnapshotRequired(SnapshotReason::DeviceRestarted { device_id })
                        if device_id == "alice"
                )
            })
            .expect("a restart published no re-baseline point");
        assert!(
            restart_at > 0 && restart_at == drained.len() - 1,
            "the re-baseline point is not where the restart finished: {restart_at} of {}",
            drained.len()
        );
    }

    /// A consumer that falls behind is told at the position it fell behind, not
    /// silently skipped forward — and the history cursor covers what it missed.
    #[tokio::test]
    async fn a_lagged_consumer_is_told_in_sequence_and_can_recover_from_history() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());
        let mut signals = supervisor.signals();

        // The broadcast buffer is 256; overrun it without reading.
        for index in 0..300 {
            supervisor.publish(EventKind::LogChanged, None, format!("filler {index}"));
        }

        let first = signals.recv().await.expect("a signal");
        assert!(
            matches!(
                first,
                ClientSignal::SnapshotRequired(SnapshotReason::ConsumerLagged { dropped }) if dropped > 0
            ),
            "a lagged consumer was skipped forward instead of told: {first:?}"
        );

        // Recovery is snapshot plus history, and history is deliberately the
        // deeper of the two: the broadcast buffer holds 256 signals and the
        // event journal holds 1,024, so everything the stream dropped is still
        // readable. A consumer told it lagged can therefore rebuild exactly,
        // rather than being told to re-baseline and finding the gap unreadable.
        let page = supervisor
            .event_history(&HistoryQuery {
                after_revision: Some(0),
                limit: Some(MAX_PAGE_LIMIT),
                ..HistoryQuery::default()
            })
            .expect("event history");
        assert!(
            !page.dropped_before,
            "history lost what the stream dropped, so a lagged consumer cannot recover"
        );
        assert_eq!(
            page.events.first().map(|event| event.revision),
            Some(1),
            "history does not reach back to the first event the consumer missed"
        );
    }

    fn peer(space: &str, id: &str) -> crate::driver::ObservedConnection {
        crate::driver::ObservedConnection {
            space_id: space.into(),
            peer_id: id.into(),
            peer_nick: id.into(),
            state: "connected".into(),
            online: true,
            dialable: true,
            blocked_by: None,
        }
    }

    /// The defect this exists to prevent: a daemon that is up but cannot be
    /// asked must not read as a daemon with no peers.
    #[tokio::test]
    async fn a_sampling_failure_is_degraded_and_never_an_empty_topology() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let fake = Fake::default();
        *lock_recovering(&fake.connections) =
            vec![peer("ws_one", "peer-a"), peer("ws_one", "peer-b")];
        let supervisor = fake_supervisor_with(alive, &fake, directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");

        let healthy = supervisor.snapshot().await;
        assert_eq!(healthy.connections.len(), 2);
        assert_eq!(
            healthy.devices[0].observation.state,
            ObservationState::Healthy
        );
        assert!(healthy.devices[0].observation.sampled_at_ms.is_some());
        assert_eq!(
            healthy.devices[0]
                .facts
                .as_ref()
                .and_then(|facts| facts.version.as_deref()),
            Some("0.0.0-test")
        );

        fake.sampling_fails.store(true, Ordering::SeqCst);
        let degraded = supervisor.snapshot().await;

        assert_eq!(
            degraded.connections.len(),
            2,
            "a sampling failure emptied the topology"
        );
        let observation = &degraded.devices[0].observation;
        assert_eq!(observation.state, ObservationState::Degraded);
        assert!(
            observation.stale_since_ms.is_some(),
            "degraded without saying since when"
        );
        assert!(
            observation.sampled_at_ms.is_some(),
            "the last good sample time was thrown away, so nothing can say how old this is"
        );
        assert!(observation.error.is_some());
        assert!(
            degraded.devices[0].facts.is_some(),
            "the last good facts were replaced by nothing"
        );

        // And no disconnection was ever published for a peer nobody watched
        // leave.
        let history = supervisor
            .connection_history(&HistoryQuery::default())
            .expect("connection history");
        assert!(
            !history
                .events
                .iter()
                .any(|event| event.kind == ConnectionEventKind::Disconnected),
            "a sampling failure published a disconnection"
        );

        // Recovery clears the staleness rather than leaving it stuck.
        fake.sampling_fails.store(false, Ordering::SeqCst);
        let recovered = supervisor.snapshot().await;
        assert_eq!(
            recovered.devices[0].observation.state,
            ObservationState::Healthy
        );
        assert!(recovered.devices[0].observation.stale_since_ms.is_none());
    }

    /// Staleness measures the beginning of the degraded stretch. If each failed
    /// attempt reset it, a surface could never say how old its figures are.
    #[tokio::test]
    async fn staleness_dates_from_the_first_failure_not_the_latest() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let fake = Fake::default();
        let supervisor = fake_supervisor_with(alive, &fake, directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");
        supervisor.observe().await;

        fake.sampling_fails.store(true, Ordering::SeqCst);
        supervisor.observe().await;
        let first = supervisor.observation_of("alice").health.stale_since_ms;
        assert!(first.is_some());

        tokio::time::sleep(Duration::from_millis(5)).await;
        supervisor.observe().await;
        assert_eq!(
            supervisor.observation_of("alice").health.stale_since_ms,
            first,
            "a second failure moved the staleness clock forward"
        );
    }

    /// A stopped device genuinely has nothing to report, which is not the same
    /// as a failure to read it.
    #[tokio::test]
    async fn a_stopped_device_is_not_a_degraded_observation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let fake = Fake::default();
        let supervisor = fake_supervisor_with(alive, &fake, directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");

        let snapshot = supervisor.snapshot().await;
        assert_eq!(snapshot.devices[0].state, LifecycleState::Stopped);
        assert_eq!(
            snapshot.devices[0].observation.state,
            ObservationState::Healthy
        );
        assert!(snapshot.devices[0].facts.is_none());
    }

    /// Correlation is by Station id, not by nickname — a nick is authored and
    /// not unique, so matching on it would let one device's label claim another.
    #[tokio::test]
    async fn an_observed_peer_is_correlated_to_a_managed_device_by_station_id() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let fake = Fake::default();
        *lock_recovering(&fake.station_id) = Some("stn_alice".into());
        *lock_recovering(&fake.connections) = vec![
            peer("ws_one", "stn_alice"),
            peer("ws_one", "stn_somebody_else"),
        ];
        let supervisor = fake_supervisor_with(alive, &fake, directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");

        // Two passes: the first learns alice's Station id, the second can use
        // it. Correlation reads facts already gathered and never asks for more.
        supervisor.observe().await;
        let snapshot = supervisor.snapshot().await;

        let managed: Vec<&ConnectionSnapshot> = snapshot
            .connections
            .iter()
            .filter(|connection| connection.target_device_id.is_some())
            .collect();
        assert_eq!(managed.len(), 1, "exactly one peer is a managed device");
        assert_eq!(managed[0].peer_id, "stn_alice");
        assert_eq!(managed[0].target_device_id.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn a_rename_survives_a_restart_and_keeps_the_running_process() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        {
            let supervisor = fake_supervisor(alive.clone(), directory.path());
            supervisor
                .create_device("alice".into(), "Alice".into())
                .await
                .expect("create device");
            let running = supervisor
                .start_device("alice")
                .await
                .expect("start device");
            assert_eq!(running.pid, Some(42));

            let renamed = supervisor
                .update_device(
                    "alice",
                    UpdateDeviceRequest {
                        label: Some("  Alice's laptop  ".into()),
                    },
                )
                .await
                .expect("rename");
            assert_eq!(renamed.label, "Alice's laptop", "label is trimmed");
            assert!(
                renamed.owned && renamed.pid == Some(42),
                "renaming stranded the owned process handle"
            );

            assert!(
                matches!(
                    supervisor
                        .update_device(
                            "alice",
                            UpdateDeviceRequest {
                                label: Some(" ".into())
                            }
                        )
                        .await,
                    Err(SupervisorError::Invalid(_))
                ),
                "an empty label was accepted"
            );
            supervisor.stop_device("alice").await.expect("stop");
        }

        let reopened = fake_supervisor(alive, directory.path());
        let snapshot = reopened.snapshot().await;
        assert_eq!(snapshot.devices[0].label, "Alice's laptop");
    }

    #[tokio::test]
    async fn removal_is_refused_while_a_device_is_running_or_external() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive.clone(), directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");
        supervisor
            .start_device("alice")
            .await
            .expect("start device");

        assert!(
            matches!(
                supervisor
                    .remove_device("alice", RemoveDeviceRequest::default())
                    .await,
                Err(SupervisorError::Conflict(_))
            ),
            "a running device was removed"
        );
        assert!(alive.load(Ordering::SeqCst), "removal stopped the daemon");

        // A daemon this supervisor did not spawn is external, and external is a
        // refusal for removal exactly as it is for force-stop.
        supervisor.stop_device("alice").await.expect("stop");
        alive.store(true, Ordering::SeqCst);
        supervisor.observe().await;
        assert!(matches!(
            supervisor
                .remove_device("alice", RemoveDeviceRequest::default())
                .await,
            Err(SupervisorError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn removal_and_deletion_are_separate_operations() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());
        for id in ["alice", "bob"] {
            supervisor
                .create_device(id.into(), id.into())
                .await
                .expect("create device");
        }
        let homes = directory.path().join("devices");

        // Forgetting a device leaves what it holds alone.
        supervisor
            .remove_device("alice", RemoveDeviceRequest::default())
            .await
            .expect("remove alice");
        assert!(
            homes.join("alice").is_dir(),
            "an unregister deleted the device's data"
        );
        assert!(matches!(
            supervisor.start_device("alice").await,
            Err(SupervisorError::NotFound(_))
        ));

        // Deleting needs to be asked for by name. A bare flag is not consent.
        assert!(matches!(
            supervisor
                .remove_device(
                    "bob",
                    RemoveDeviceRequest {
                        delete_data: true,
                        confirm: None,
                    },
                )
                .await,
            Err(SupervisorError::Invalid(_))
        ));
        assert!(matches!(
            supervisor
                .remove_device(
                    "bob",
                    RemoveDeviceRequest {
                        delete_data: true,
                        confirm: Some("alice".into()),
                    },
                )
                .await,
            Err(SupervisorError::Invalid(_))
        ));
        assert!(homes.join("bob").is_dir(), "a refused delete still deleted");

        supervisor
            .remove_device(
                "bob",
                RemoveDeviceRequest {
                    delete_data: true,
                    confirm: Some("bob".into()),
                },
            )
            .await
            .expect("delete bob");
        assert!(!homes.join("bob").exists(), "confirmed delete left data");
        assert!(
            homes.join("alice").is_dir(),
            "deleting one device took another's data with it"
        );
    }

    /// The managed root is the containment boundary, and it is re-established at
    /// the moment of deletion rather than trusted from registration time.
    #[tokio::test]
    async fn data_outside_the_managed_root_is_refused_rather_than_deleted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let alive = Arc::new(AtomicBool::new(false));
        let supervisor = fake_supervisor(alive, directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("create device");

        let escaped = std::fs::canonicalize(elsewhere.path()).expect("canonical elsewhere");
        let device = supervisor.device("alice").await.expect("device");
        let contained = Arc::new(Device {
            id: device.id.clone(),
            label: StdMutex::new(device.label()),
            home: escaped.clone(),
            runtime: Mutex::new(Runtime {
                state: LifecycleState::Stopped,
                process: None,
                started_at_ms: None,
                last_error: None,
            }),
        });
        assert!(
            matches!(
                supervisor.contained_home(&contained),
                Err(SupervisorError::Invalid(_))
            ),
            "a home outside the managed root passed containment"
        );
        assert!(escaped.is_dir(), "the refused path was deleted anyway");

        // The root itself is not a device home, whatever a registry claims.
        let rooted = Arc::new(Device {
            id: device.id.clone(),
            label: StdMutex::new(device.label()),
            home: supervisor.inner.state_root.clone(),
            runtime: Mutex::new(Runtime {
                state: LifecycleState::Stopped,
                process: None,
                started_at_ms: None,
                last_error: None,
            }),
        });
        assert!(matches!(
            supervisor.contained_home(&rooted),
            Err(SupervisorError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn never_force_stops_a_process_it_did_not_spawn() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(true));
        let supervisor = fake_supervisor(alive.clone(), directory.path());
        supervisor
            .create_device("bob".into(), "Bob".into())
            .await
            .expect("add device");

        assert!(matches!(
            supervisor.start_device("bob").await,
            Err(SupervisorError::Conflict(_))
        ));
        assert!(matches!(
            supervisor.force_stop_device("bob").await,
            Err(SupervisorError::Conflict(_))
        ));
        assert!(alive.load(Ordering::SeqCst));
        let snapshot = supervisor.snapshot().await;
        assert_eq!(snapshot.devices[0].state, LifecycleState::External);
    }

    #[tokio::test]
    async fn device_definitions_survive_a_supervisor_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        {
            let supervisor = fake_supervisor(alive.clone(), directory.path());
            supervisor
                .create_device("alice".into(), "Alice".into())
                .await
                .expect("add device");
        }

        let reopened = fake_supervisor(alive, directory.path());
        let snapshot = reopened.snapshot().await;
        assert_eq!(snapshot.devices.len(), 1);
        assert_eq!(snapshot.devices[0].id, "alice");
        assert_eq!(snapshot.devices[0].label, "Alice");
        assert_eq!(snapshot.devices[0].state, LifecycleState::Stopped);
    }

    #[tokio::test]
    async fn a_daemon_surviving_restart_is_discovered_but_never_adopted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(false));
        {
            let supervisor = fake_supervisor(alive.clone(), directory.path());
            supervisor
                .create_device("alice".into(), "Alice".into())
                .await
                .expect("add device");
        }
        alive.store(true, Ordering::SeqCst);

        let reopened = fake_supervisor(alive.clone(), directory.path());
        reopened.reconcile().await;
        let snapshot = reopened.snapshot().await;
        assert_eq!(snapshot.devices[0].state, LifecycleState::External);
        assert!(!snapshot.devices[0].owned);
        assert!(matches!(
            reopened.force_stop_device("alice").await,
            Err(SupervisorError::Conflict(_))
        ));
        assert!(alive.load(Ordering::SeqCst));
    }

    #[test]
    fn event_history_is_bounded_and_reports_a_dropped_cursor() {
        let directory = tempfile::tempdir().expect("tempdir");
        let supervisor = fake_supervisor(Arc::new(AtomicBool::new(false)), directory.path());
        for number in 0..EVENT_HISTORY_CAPACITY.saturating_add(3) {
            supervisor.publish(EventKind::LifecycleChanged, None, format!("event {number}"));
        }
        let page = supervisor
            .event_history(&HistoryQuery {
                after_revision: Some(0),
                limit: Some(1),
                ..HistoryQuery::default()
            })
            .expect("history");
        assert_eq!(page.oldest_available_revision, Some(4));
        assert!(page.dropped_before);
        assert!(page.has_more);
        assert_eq!(page.events.len(), 1);
    }

    #[tokio::test]
    async fn connection_history_records_only_transitions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(true));
        let connections = Arc::new(StdMutex::new(vec![crate::driver::ObservedConnection {
            space_id: "space-one".into(),
            peer_id: "peer-one".into(),
            peer_nick: "Peer One".into(),
            state: "connected".into(),
            online: true,
            dialable: true,
            blocked_by: None,
        }]));
        let supervisor =
            fake_supervisor_with_connections(alive, connections.clone(), directory.path());
        supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("add device");

        supervisor.observe().await;
        supervisor.observe().await;
        lock_recovering(&connections)[0].online = false;
        supervisor.observe().await;
        lock_recovering(&connections).clear();
        supervisor.observe().await;

        let page = supervisor
            .connection_history(&HistoryQuery::default())
            .expect("history");
        assert_eq!(page.events.len(), 3);
        assert!(!page.dropped_before);
        assert_eq!(page.events[0].kind, ConnectionEventKind::Connected);
        assert_eq!(page.events[1].kind, ConnectionEventKind::Changed);
        assert_eq!(page.events[2].kind, ConnectionEventKind::Disconnected);
    }

    #[tokio::test]
    async fn log_pages_are_structured_and_log_growth_is_an_event() {
        let directory = tempfile::tempdir().expect("tempdir");
        let supervisor = fake_supervisor(Arc::new(AtomicBool::new(false)), directory.path());
        let device = supervisor
            .create_device("alice".into(), "Alice".into())
            .await
            .expect("add device");
        supervisor.observe().await;
        let log_path = PathBuf::from(device.log_path);
        std::fs::create_dir_all(log_path.parent().expect("log parent")).expect("create log parent");
        std::fs::write(
            &log_path,
            b"\x1b[2m2026-08-09T23:02:54.347526Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mlait::daemon::host\x1b[0m\x1b[2m:\x1b[0m online\n",
        )
        .expect("write log");
        supervisor.observe().await;

        let page = supervisor
            .logs("alice", None, Some(20))
            .await
            .expect("logs");
        assert_eq!(page.entries.len(), 1);
        assert!(matches!(page.entries[0].level, crate::LogLevel::Info));
        assert_eq!(
            page.entries[0].target.as_deref(),
            Some("lait::daemon::host")
        );
        let history = supervisor
            .event_history(&HistoryQuery::default())
            .expect("history");
        assert!(history
            .events
            .iter()
            .any(|event| matches!(event.kind, EventKind::LogChanged)));
    }

    #[test]
    fn observability_page_limits_are_validated() {
        assert!(matches!(
            history_limit(Some(0)),
            Err(SupervisorError::Invalid(_))
        ));
        assert!(matches!(
            history_limit(Some(MAX_PAGE_LIMIT.saturating_add(1))),
            Err(SupervisorError::Invalid(_))
        ));
        assert_eq!(history_limit(None).expect("default"), DEFAULT_PAGE_LIMIT);
    }

    #[cfg(unix)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }
}
