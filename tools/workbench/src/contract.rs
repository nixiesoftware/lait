use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub environment: EnvironmentSnapshot,
    pub capabilities: Capabilities,
    pub devices: Vec<DeviceSnapshot>,
    pub connections: Vec<ConnectionSnapshot>,
    /// The image this supervisor is currently spawning from. What a device's
    /// own [`DeviceSnapshot::image`] is compared against to say "this node is
    /// running older code than the bench would start today". `None` when
    /// nothing was ever staged — a bare supervisor in a test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageFacts>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshot {
    pub state_root: String,
    pub executable: String,
    pub server_pid: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub create_device: bool,
    pub update_device: bool,
    pub remove_device: bool,
    pub delete_device_data: bool,
    pub start: bool,
    pub stop: bool,
    pub restart: bool,
    pub force_stop_owned_process: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            create_device: true,
            update_device: true,
            remove_device: true,
            delete_device_data: true,
            start: true,
            stop: true,
            restart: true,
            force_stop_owned_process: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub id: String,
    pub label: String,
    pub home: String,
    pub log_path: String,
    pub state: LifecycleState,
    pub pid: Option<u32>,
    pub owned: bool,
    pub started_at_ms: Option<u64>,
    pub last_error: Option<String>,
    /// What the daemon said about itself when it was last successfully asked.
    /// `None` means nobody has ever got an answer — not that the answer was
    /// empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<DeviceFacts>,
    /// Whether the figures above are current, and since when they were not.
    pub observation: ObservationHealth,
    /// The image this device was spawned from. A staged run may outlive the
    /// tree that produced it, so this is reported rather than inferred from the
    /// workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageFacts>,
}

/// Which image a device is actually running.
///
/// Reported rather than assumed, because a staged run may outlive the tree that
/// produced it: the workspace can be rebuilt, moved or deleted while a daemon
/// started from a copy of an older binary keeps serving. A client that shows the
/// source path in that situation is lying about what is under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageFacts {
    /// Where the image was copied from.
    pub source_path: String,
    /// The copy actually executed. Equal to `source_path` when staging is off,
    /// which is the packaged client's case.
    pub staged_path: String,
    /// Content hash of the staged bytes. Two devices reporting the same
    /// fingerprint are running the same code, whatever their paths say.
    pub fingerprint: String,
    pub staged_at_ms: u64,
}

/// What a running daemon reports about itself.
///
/// Every field is optional because a daemon may answer partially, and a field
/// nobody could read is absent rather than defaulted. A zero that means "not
/// measured" is indistinguishable from a zero that was measured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_client_url: Option<String>,
    pub spaces: Vec<String>,
}

/// Whether an observation is current, and what went wrong if it is not.
///
/// This exists so a surface can tell "there are no peers" apart from "nobody
/// could ask". Rendering the second as the first is the defect the release gate
/// tests for directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ObservationHealth {
    pub state: ObservationState,
    /// When the last *successful* sample completed. `None` before the first one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampled_at_ms: Option<u64>,
    /// When sampling started failing — the beginning of the degraded stretch,
    /// not the most recent attempt, so a surface can say how old the figures are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_since_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for ObservationHealth {
    /// Never sampled: healthy, because nothing has failed, and with no
    /// `sampled_at_ms`, because nothing has succeeded either.
    fn default() -> Self {
        Self {
            state: ObservationState::Healthy,
            sampled_at_ms: None,
            stale_since_ms: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationState {
    Healthy,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Stopped,
    Starting,
    Running,
    Stopping,
    External,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSnapshot {
    pub source_device_id: String,
    pub space_id: String,
    pub peer_id: String,
    pub peer_nick: String,
    pub state: String,
    pub online: bool,
    pub dialable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    /// The managed device on the other end, when this peer is one of ours.
    ///
    /// Correlation is by the Station id a daemon reports for itself, never by
    /// nickname: a nick is authored, is not unique, and naming one device by
    /// another's label is how a topology view starts lying.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventHistoryPage {
    pub schema_version: u32,
    pub oldest_available_revision: Option<u64>,
    pub newest_revision: u64,
    pub next_revision: u64,
    pub dropped_before: bool,
    pub has_more: bool,
    pub events: Vec<BackendEvent>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionHistoryPage {
    pub schema_version: u32,
    pub oldest_available_revision: Option<u64>,
    pub newest_revision: u64,
    pub next_revision: u64,
    pub dropped_before: bool,
    pub has_more: bool,
    pub events: Vec<ConnectionEvent>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEvent {
    pub revision: u64,
    pub at_ms: u64,
    pub kind: ConnectionEventKind,
    pub connection: ConnectionSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionEventKind {
    Connected,
    Changed,
    Disconnected,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryQuery {
    pub after_revision: Option<u64>,
    pub limit: Option<usize>,
    pub device_id: Option<String>,
    pub space_id: Option<String>,
    pub peer_id: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogQuery {
    pub cursor: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub schema_version: u32,
    pub device_id: String,
    pub file_size: u64,
    pub next_cursor: u64,
    pub reset: bool,
    pub has_more: bool,
    pub entries: Vec<LogEntry>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub cursor: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub level: LogLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub message: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Unknown,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDeviceRequest {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub start: bool,
}

/// A change to a registration. Absent fields are left alone, so a caller that
/// only means to rename cannot accidentally reset anything else.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateDeviceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Removal, and separately, deletion.
///
/// `delete_data` is the whole difference between forgetting a device and
/// destroying what it holds, so it is a field a caller states rather than a
/// consequence of which route they reached. It is refused unless the device is
/// stopped, its home canonicalizes to somewhere beneath the managed state root,
/// and `confirm` names the device being destroyed.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveDeviceRequest {
    #[serde(default)]
    pub delete_data: bool,
    /// The device id again, typed back by whoever is asking. A boolean is not
    /// a confirmation — it is a default somebody can send by accident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceAction {
    Start,
    Stop,
    Restart,
    ForceStop,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackendEvent {
    pub revision: u64,
    pub at_ms: u64,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    DeviceAdded,
    DeviceUpdated,
    DeviceRemoved,
    LifecycleChanged,
    ConnectionChanged,
    LogChanged,
    SnapshotRequired,
}

/// Everything a consumer of this library learns about, in one stream of one
/// type.
///
/// One stream rather than several is deliberate. Separate channels have no
/// defined ordering between them, so a `SnapshotRequired` could be observed
/// before the event that caused it and a consumer could not tell which side of
/// the gap it was on. A single stream makes the recovery point unambiguous:
/// everything before a `SnapshotRequired` is accounted for, everything after it
/// is current, and the consumer re-baselines exactly once.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum ClientSignal {
    /// The existing revisioned event. Revisions are monotonic across the
    /// supervisor's whole life, including across a supervised process restart.
    Event(BackendEvent),
    /// This consumer's view is no longer derivable from what it has seen.
    /// Fetch a snapshot; the history cursor covers what was missed.
    SnapshotRequired(SnapshotReason),
    /// A World asking the client for an ambient service. The variant exists
    /// here, rather than on a channel of its own, so a call's ordering against
    /// the events that caused it is defined. Production is CLIENT-19.
    WorldCall(WorldCall),
}

impl ClientSignal {
    /// The signal a consumer that fell behind receives, wherever it is noticed.
    ///
    /// One constructor because both the library's own `recv` and the HTTP
    /// adapter must say the same thing about the same condition; two spellings
    /// of "you lost events" is how the two ends stop agreeing on what a
    /// consumer has to re-read.
    pub fn lagged(dropped: u64) -> Self {
        Self::SnapshotRequired(SnapshotReason::ConsumerLagged { dropped })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SnapshotReason {
    /// The consumer fell far enough behind that the buffer dropped events.
    /// Reported at the position in the stream where the loss happened, which is
    /// the only place it can be reported without ambiguity.
    ConsumerLagged { dropped: u64 },
    /// A supervised daemon came back, so device-derived state may have moved in
    /// ways individual events do not describe.
    DeviceRestarted { device_id: String },
    /// The fleet was stopped, rebuilt, restaged and restarted. Every device's
    /// image may have changed underneath whatever a consumer last read.
    Reloaded,
}

/// A World's request for an ambient service.
///
/// The caller is named rather than inferred: a World's backend calls
/// in-process and a World's page calls over the head's session channel, and
/// those are not the same trust.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorldCall {
    pub world: String,
    pub caller: WorldCaller,
    pub capability: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorldCaller {
    Backend,
    Page,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error: &'static str,
    pub message: String,
}

/// The complete machine-readable boundary a client can generate bindings from.
///
/// The route table and the schemas travel together so a generated client never
/// has to copy either half out of prose. `schemaVersion` changes when a client
/// must change; additive fields remain within the current version.
pub fn schema_bundle() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": SCHEMA_VERSION,
        "routes": {
            "openSession": { "method": "POST", "path": "/api/workbench/session" },
            "contract": { "method": "GET", "path": "/api/workbench/contract" },
            "snapshot": { "method": "GET", "path": "/api/workbench/snapshot" },
            "events": { "method": "GET", "path": "/api/workbench/events" },
            "eventHistory": {
                "method": "GET",
                "path": "/api/workbench/history/events"
            },
            "connectionHistory": {
                "method": "GET",
                "path": "/api/workbench/history/connections"
            },
            "deviceLogs": {
                "method": "GET",
                "path": "/api/workbench/devices/{id}/logs"
            },
            "createDevice": { "method": "POST", "path": "/api/workbench/devices" },
            "updateDevice": {
                "method": "PATCH",
                "path": "/api/workbench/devices/{id}"
            },
            "removeDevice": {
                "method": "DELETE",
                "path": "/api/workbench/devices/{id}"
            },
            "deviceAction": {
                "method": "POST",
                "path": "/api/workbench/devices/{id}/actions"
            }
        },
        "schemas": {
            "snapshot": schemars::schema_for!(WorkbenchSnapshot),
            "createDeviceRequest": schemars::schema_for!(CreateDeviceRequest),
            "updateDeviceRequest": schemars::schema_for!(UpdateDeviceRequest),
            "removeDeviceRequest": schemars::schema_for!(RemoveDeviceRequest),
            "deviceAction": schemars::schema_for!(DeviceAction),
            "event": schemars::schema_for!(BackendEvent),
            "historyQuery": schemars::schema_for!(HistoryQuery),
            "eventHistory": schemars::schema_for!(EventHistoryPage),
            "connectionHistory": schemars::schema_for!(ConnectionHistoryPage),
            "logQuery": schemars::schema_for!(LogQuery),
            "logs": schemars::schema_for!(LogPage),
            "error": schemars::schema_for!(ApiError)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_bundle_names_every_public_route_and_schema() {
        let bundle = schema_bundle();
        assert_eq!(bundle["schemaVersion"], SCHEMA_VERSION);
        assert_eq!(
            bundle["routes"]["snapshot"]["path"],
            "/api/workbench/snapshot"
        );
        for route in [
            "openSession",
            "contract",
            "snapshot",
            "events",
            "eventHistory",
            "connectionHistory",
            "deviceLogs",
            "createDevice",
            "deviceAction",
        ] {
            assert!(bundle["routes"].get(route).is_some(), "{route}");
        }
        for schema in [
            "snapshot",
            "createDeviceRequest",
            "deviceAction",
            "event",
            "historyQuery",
            "eventHistory",
            "connectionHistory",
            "logQuery",
            "logs",
            "error",
        ] {
            assert!(bundle["schemas"].get(schema).is_some(), "{schema}");
        }
    }

    #[test]
    fn lifecycle_wire_names_are_pinned() {
        let names = [
            (LifecycleState::Stopped, "stopped"),
            (LifecycleState::Starting, "starting"),
            (LifecycleState::Running, "running"),
            (LifecycleState::Stopping, "stopping"),
            (LifecycleState::External, "external"),
            (LifecycleState::Failed, "failed"),
        ];
        for (state, expected) in names {
            assert_eq!(serde_json::to_value(state).expect("serialize"), expected);
        }
    }
}
