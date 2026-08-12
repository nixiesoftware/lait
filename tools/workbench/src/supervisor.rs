use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, Mutex, RwLock};

use crate::contract::{
    BackendEvent, Capabilities, ConnectionEvent, ConnectionEventKind, ConnectionHistoryPage,
    ConnectionSnapshot, DeviceSnapshot, EnvironmentSnapshot, EventHistoryPage, EventKind,
    HistoryQuery, LifecycleState, LogPage, WorkbenchSnapshot, SCHEMA_VERSION,
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
    events: broadcast::Sender<BackendEvent>,
    history: StdMutex<HistoryState>,
    observed_connections: StdMutex<BTreeMap<ConnectionKey, ConnectionSnapshot>>,
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
    label: String,
    home: PathBuf,
    runtime: Mutex<Runtime>,
}

struct Runtime {
    state: LifecycleState,
    process: Option<Box<dyn OwnedDaemon>>,
    started_at_ms: Option<u64>,
    last_error: Option<String>,
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
    pub async fn start(config: Config) -> Result<Self, SupervisorError> {
        let supervisor = Self::new(config.state_root, config.executable)?;
        supervisor.observe().await;
        supervisor.observe_in_background(config.observation_interval);
        Ok(supervisor)
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
            devices.insert(id, create_device(&state_root, registration)?);
        }
        let (events, _) = broadcast::channel(256);
        Ok(Self {
            inner: Arc::new(Inner {
                state_root,
                executable,
                driver,
                registry,
                devices: RwLock::new(devices),
                revision: AtomicU64::new(0),
                events,
                history: StdMutex::new(HistoryState::default()),
                observed_connections: StdMutex::new(BTreeMap::new()),
                observed_log_sizes: StdMutex::new(BTreeMap::new()),
                observation: Mutex::new(()),
                observer: StdMutex::new(None),
                start_timeout,
                stop_timeout,
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BackendEvent> {
        self.inner.events.subscribe()
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
        let mut current_connections = BTreeMap::new();
        let mut current_log_sizes = BTreeMap::new();
        for device in devices {
            let mut runtime = device.runtime.lock().await;
            self.refresh_owned_process(&device, &mut runtime);
            let inspect_connections = matches!(
                runtime.state,
                LifecycleState::Running | LifecycleState::External
            );
            drop(runtime);

            if inspect_connections {
                for connection in self.inner.driver.connections(&device.home).await {
                    let snapshot = ConnectionSnapshot {
                        source_device_id: device.id.clone(),
                        space_id: connection.space_id,
                        peer_id: connection.peer_id,
                        peer_nick: connection.peer_nick,
                        state: connection.state,
                        online: connection.online,
                        dialable: connection.dialable,
                        blocked_by: connection.blocked_by,
                    };
                    current_connections.insert(connection_key(&snapshot), snapshot);
                }
            }

            let log_size = std::fs::metadata(device_log_path(&device))
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            current_log_sizes.insert(device.id.clone(), log_size);
        }
        self.record_connection_observations(current_connections);
        self.record_log_observations(current_log_sizes);
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

    pub async fn add_device(
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
        let device = create_device(&self.inner.state_root, registration.clone())?;
        {
            let mut devices = self.inner.devices.write().await;
            if devices.contains_key(&id) {
                return Err(SupervisorError::AlreadyExists(format!(
                    "device '{id}' already exists"
                )));
            }
            let mut registrations: Vec<RegisteredDevice> = devices
                .values()
                .map(|device| RegisteredDevice {
                    id: device.id.clone(),
                    label: device.label.clone(),
                })
                .collect();
            registrations.push(registration);
            registrations.sort_by(|left, right| left.id.cmp(&right.id));
            self.inner.registry.save(&registrations)?;
            devices.insert(id.clone(), device.clone());
        }
        self.publish(EventKind::DeviceAdded, Some(id), "device added");
        let runtime = device.runtime.lock().await;
        Ok(snapshot_device(&device, &runtime))
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
                return Ok(snapshot_device(&device, &runtime));
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
            return Ok(snapshot_device(&device, &runtime));
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
                return Ok(snapshot_device(&device, &runtime));
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
        Ok(snapshot_device(&device, &runtime))
    }

    pub async fn restart_device(&self, id: &str) -> Result<DeviceSnapshot, SupervisorError> {
        self.stop_device(id).await?;
        self.start_device(id).await
    }

    pub async fn snapshot(&self) -> WorkbenchSnapshot {
        self.observe().await;
        let devices: Vec<Arc<Device>> = self.inner.devices.read().await.values().cloned().collect();
        let mut snapshots = Vec::with_capacity(devices.len());
        for device in devices {
            let mut runtime = device.runtime.lock().await;
            self.refresh_owned_process(&device, &mut runtime);
            let snapshot = snapshot_device(&device, &runtime);
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

    fn record_connection_observations(&self, current: BTreeMap<ConnectionKey, ConnectionSnapshot>) {
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
            for (key, connection) in previous.iter() {
                if !current.contains_key(key) {
                    changes.push((ConnectionEventKind::Disconnected, connection.clone()));
                }
            }
            *previous = current;
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
        let _ = self.inner.events.send(backend_event);
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
        let _ = self.inner.events.send(event);
    }

    fn next_revision(&self) -> u64 {
        self.inner
            .revision
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }
}

fn snapshot_device(device: &Device, runtime: &Runtime) -> DeviceSnapshot {
    DeviceSnapshot {
        id: device.id.clone(),
        label: device.label.clone(),
        home: path_text(&device.home),
        log_path: path_text(&device_log_path(device)),
        state: runtime.state,
        pid: runtime.process.as_ref().map(|process| process.id()),
        owned: runtime.process.is_some(),
        started_at_ms: runtime.started_at_ms,
        last_error: runtime.last_error.clone(),
    }
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

fn create_device(
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
        label: registration.label,
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

        async fn connections(&self, _home: &Path) -> Vec<crate::driver::ObservedConnection> {
            lock_recovering(&self.connections).clone()
        }
    }

    fn fake_supervisor(alive: Arc<AtomicBool>, root: &Path) -> Supervisor {
        fake_supervisor_with_connections(alive, Arc::new(StdMutex::new(Vec::new())), root)
    }

    fn fake_supervisor_with_connections(
        alive: Arc<AtomicBool>,
        connections: Arc<StdMutex<Vec<crate::driver::ObservedConnection>>>,
        root: &Path,
    ) -> Supervisor {
        let root = std::fs::canonicalize(root).expect("canonical test root");
        Supervisor::with_driver(
            root,
            PathBuf::from("fake-lait"),
            Arc::new(FakeDriver { alive, connections }),
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
            .add_device("alice".into(), "Alice's laptop".into())
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
            .add_device("alice".into(), "Alice".into())
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

    #[tokio::test]
    async fn never_force_stops_a_process_it_did_not_spawn() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alive = Arc::new(AtomicBool::new(true));
        let supervisor = fake_supervisor(alive.clone(), directory.path());
        supervisor
            .add_device("bob".into(), "Bob".into())
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
                .add_device("alice".into(), "Alice".into())
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
                .add_device("alice".into(), "Alice".into())
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
            .add_device("alice".into(), "Alice".into())
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
            .add_device("alice".into(), "Alice".into())
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
