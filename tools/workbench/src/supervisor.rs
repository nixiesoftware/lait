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
    EventHistoryPage, EventKind, HistoryQuery, LifecycleState, LogPage, ObservationHealth,
    ObservationState, RemoveDeviceRequest, SnapshotReason, UpdateDeviceRequest, WorkbenchSnapshot,
    SCHEMA_VERSION,
};
use crate::driver::{DaemonDriver, DaemonProbe, LaitDriver, OwnedDaemon};
use crate::observability::{read_log_page, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT};
use crate::registry::{RegisteredDevice, Registry};

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
}

impl Config {
    /// A configuration sampling at [`OBSERVATION_INTERVAL`].
    pub fn new(state_root: PathBuf, executable: PathBuf) -> Self {
        Self {
            state_root,
            executable,
            observation_interval: OBSERVATION_INTERVAL,
        }
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
    start_timeout: Duration,
    stop_timeout: Duration,
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
        let supervisor = Self::new(config.state_root, config.executable)?;
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
                start_timeout,
                stop_timeout,
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
            let snapshot = snapshot_device(&device, &runtime, self.observation_of(&device.id));
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
        }
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
        let (supervisor, mut signals) = Supervisor::start(Config {
            state_root: root,
            executable: PathBuf::from("fake-lait"),
            observation_interval: Duration::from_secs(3_600),
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
