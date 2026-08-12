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
    pub start: bool,
    pub stop: bool,
    pub restart: bool,
    pub force_stop_owned_process: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            create_device: true,
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

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
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

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    DeviceAdded,
    LifecycleChanged,
    ConnectionChanged,
    LogChanged,
    SnapshotRequired,
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
            "deviceAction": {
                "method": "POST",
                "path": "/api/workbench/devices/{id}/actions"
            }
        },
        "schemas": {
            "snapshot": schemars::schema_for!(WorkbenchSnapshot),
            "createDeviceRequest": schemars::schema_for!(CreateDeviceRequest),
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
