//! The desktop host is deliberately only an adapter.
//!
//! Astrolabe's Rust core retains the application model and action semantics.
//! This process starts that core, serializes the primary-window projection for
//! the WebView, and forwards its already-existing whole-view stream.

use astrolabe::api::{self, ActionRequest, ClientView, Staleness};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const CLIENT_VIEW_EVENT: &str = "astrolabe://client-view";

// A browser preview must not infer desktop identity from its user agent. The
// host owns this fact and installs it before the document's application code
// runs, so each operating system can express its native window treatment.
#[cfg(target_os = "macos")]
const PLATFORM_INIT: &str = "window.__ASTROLABE_PLATFORM__ = 'macos';";
#[cfg(target_os = "windows")]
const PLATFORM_INIT: &str = "window.__ASTROLABE_PLATFORM__ = 'windows';";
#[cfg(target_os = "linux")]
const PLATFORM_INIT: &str = "window.__ASTROLABE_PLATFORM__ = 'linux';";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebClientView {
    loading: bool,
    stale: Option<WebStaleness>,
    library: Option<Vec<WebLibraryWorld>>,
    host: Option<WebHostFacts>,
    display: Option<WebDisplayFacts>,
    heads: Vec<WebHead>,
    devices: Vec<WebDevice>,
    storage: Vec<WebStorage>,
    orbits: Vec<WebOrbit>,
    space: Option<WebSpace>,
    book: Option<WebBook>,
    mcp: Option<WebMcpBinding>,
    notices: Vec<WebNotice>,
    failures: Vec<WebFailure>,
    in_flight: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebStaleness {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebLibraryWorld {
    key: String,
    world_mount: String,
    display_name: String,
    opens_at: Option<String>,
    version: Option<u32>,
    tagline: Option<String>,
    accent: Option<u32>,
    people: Option<Vec<WebWorldPerson>>,
    update: Option<WebWorldUpdate>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebWorldPerson {
    name: String,
    picture: Option<String>,
    presence: Option<&'static str>,
    agent: bool,
    here: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebWorldUpdate {
    serving: Option<String>,
    available: Option<String>,
    behind: bool,
    unmet: Option<Vec<String>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebHostFacts {
    version: String,
    identity_home: String,
    spaces_root: String,
    orbit_count: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebHead {
    id: String,
    kind: String,
    origin: Option<String>,
    owned: bool,
    orbit: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebNotice {
    said: String,
    launched: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebFailure {
    what: String,
    error: String,
    retryable: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDevice {
    id: String,
    label: String,
    state: String,
    owned: bool,
    degraded: Option<String>,
    home: String,
    pid: Option<u32>,
    can_force_stop: bool,
    last_error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebStorage {
    orbit: String,
    name: Option<String>,
    bytes_on_disk: Option<u64>,
    object_count: Option<u64>,
    last_verified_ms: Option<u64>,
    missing: Option<&'static str>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebOrbit {
    space: String,
    name: String,
    path: String,
    last_opened: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebSpace {
    space: String,
    whoami: Option<String>,
    admin: bool,
    members: Vec<WebMember>,
    devices: Vec<String>,
    diagnosis: Option<WebDiagnosis>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebMember {
    id: String,
    nick: Option<String>,
    authored_name: Option<String>,
    admin: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDiagnosis {
    gates: Vec<WebGate>,
    blocked_on: Option<String>,
    summary: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebGate {
    id: String,
    label: String,
    state: &'static str,
    detail: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebBook {
    cards: Vec<WebCard>,
    migration_complete: bool,
    migration_pending: u32,
    migration_imported: u32,
    suggestions: Vec<WebSuggestion>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebCard {
    card: String,
    name: String,
    note: String,
    handles: Vec<String>,
    addresses: Vec<String>,
    devices: Vec<String>,
    agents: Vec<String>,
    picture: Option<String>,
    groups: Vec<String>,
    self_claim: bool,
    presence: Option<&'static str>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebSuggestion {
    suggestion: String,
    name: String,
    note: String,
    handles: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebMcpBinding {
    path: String,
    detail: String,
    note: Option<String>,
    replaced: bool,
    agent: Option<String>,
    written: bool,
    world: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDisplayFacts {
    instance: String,
    label: String,
    origin: String,
    certificate_sha256: String,
    certificate_pem: String,
    surfaces: Vec<WebDisplaySurface>,
    devices: Vec<WebDisplayReceiver>,
    assignments: Vec<WebDisplayAssignment>,
    pending_pairings: Vec<WebDisplayPairing>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDisplaySurface {
    world: String,
    surface: String,
    title: String,
    contract_version: u32,
    outputs: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDisplayReceiver {
    device: String,
    label: String,
    platform: String,
    build: String,
    issued_at_unix_ms: u64,
    revoked_at_unix_ms: Option<u64>,
    health: Option<WebDisplayHealth>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDisplayHealth {
    revision: String,
    current_item: String,
    elapsed_ms: u32,
    connection: String,
    playback: String,
    last_error: String,
    staged_items: u16,
    staged_bytes: u32,
    drift_residual_ms: i32,
    correction_events: u32,
    pipeline_unobservable: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDisplayAssignment {
    assignment: String,
    device: String,
    orbit: String,
    space: String,
    program: String,
    world: String,
    surface: String,
    controller: String,
    theme: &'static str,
    sync_group: Option<String>,
    sync_mode: Option<&'static str>,
    static_delay_ms: i32,
    expires_at_unix_ms: Option<u64>,
    revoked_at_unix_ms: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDisplayPairing {
    pairing: String,
    confirmation_phrase: Vec<String>,
    certificate_sha256: String,
    platform: String,
    build: String,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
}

impl From<ClientView> for WebClientView {
    fn from(view: ClientView) -> Self {
        Self {
            loading: view.loading,
            stale: view.stale.map(|stale| match stale {
                Staleness::NeverLoaded => WebStaleness {
                    kind: "neverLoaded",
                    reason: None,
                },
                Staleness::Signalled(reason) => WebStaleness {
                    kind: "signalled",
                    reason: Some(reason),
                },
            }),
            library: view.library.map(|rows| {
                rows.into_iter()
                    .map(|row| WebLibraryWorld {
                        key: row.key,
                        world_mount: row.world_mount,
                        display_name: row.display_name,
                        opens_at: row.opens_at,
                        version: row.version,
                        tagline: row.tagline,
                        accent: row.accent,
                        people: row.people.map(|people| {
                            people
                                .into_iter()
                                .map(|person| WebWorldPerson {
                                    name: person.name,
                                    picture: person.picture,
                                    presence: person.presence.map(|presence| match presence {
                                        api::PresenceView::Online => "online",
                                        api::PresenceView::Away => "away",
                                        api::PresenceView::Offline => "offline",
                                    }),
                                    agent: person.agent,
                                    here: person.here,
                                })
                                .collect()
                        }),
                        update: row.update.map(|update| WebWorldUpdate {
                            serving: update.serving,
                            available: update.available,
                            behind: update.behind,
                            unmet: update.unmet,
                        }),
                    })
                    .collect()
            }),
            host: view.host.map(|host| WebHostFacts {
                version: host.version,
                identity_home: host.identity_home,
                spaces_root: host.spaces_root,
                orbit_count: host.orbit_count,
            }),
            display: view.display.map(|display| WebDisplayFacts {
                instance: display.instance,
                label: display.label,
                origin: display.origin,
                certificate_sha256: display.certificate_sha256,
                certificate_pem: display.certificate_pem,
                surfaces: display
                    .surfaces
                    .into_iter()
                    .map(|surface| WebDisplaySurface {
                        world: surface.world,
                        surface: surface.surface,
                        title: surface.title,
                        contract_version: surface.contract_version,
                        outputs: surface.outputs,
                    })
                    .collect(),
                devices: display
                    .devices
                    .into_iter()
                    .map(|device| WebDisplayReceiver {
                        device: device.device,
                        label: device.label,
                        platform: device.platform,
                        build: device.build,
                        issued_at_unix_ms: device.issued_at_unix_ms,
                        revoked_at_unix_ms: device.revoked_at_unix_ms,
                        health: device.health.map(|health| WebDisplayHealth {
                            revision: health.revision,
                            current_item: health.current_item,
                            elapsed_ms: health.elapsed_ms,
                            connection: health.connection,
                            playback: health.playback,
                            last_error: health.last_error,
                            staged_items: health.staged_items,
                            staged_bytes: health.staged_bytes,
                            drift_residual_ms: health.drift_residual_ms,
                            correction_events: health.correction_events,
                            pipeline_unobservable: health.pipeline_unobservable,
                        }),
                    })
                    .collect(),
                assignments: display
                    .assignments
                    .into_iter()
                    .map(|assignment| WebDisplayAssignment {
                        assignment: assignment.assignment,
                        device: assignment.device,
                        orbit: assignment.orbit,
                        space: assignment.space,
                        program: assignment.program,
                        world: assignment.world,
                        surface: assignment.surface,
                        controller: assignment.controller,
                        theme: display_theme_name(assignment.theme),
                        sync_group: assignment.sync_group,
                        sync_mode: assignment.sync_mode.map(display_sync_mode_name),
                        static_delay_ms: assignment.static_delay_ms,
                        expires_at_unix_ms: assignment.expires_at_unix_ms,
                        revoked_at_unix_ms: assignment.revoked_at_unix_ms,
                    })
                    .collect(),
                pending_pairings: display
                    .pending_pairings
                    .into_iter()
                    .map(|pairing| WebDisplayPairing {
                        pairing: pairing.pairing,
                        confirmation_phrase: pairing.confirmation_phrase,
                        certificate_sha256: pairing.certificate_sha256,
                        platform: pairing.platform,
                        build: pairing.build,
                        created_at_unix_ms: pairing.created_at_unix_ms,
                        expires_at_unix_ms: pairing.expires_at_unix_ms,
                    })
                    .collect(),
            }),
            heads: view
                .heads
                .into_iter()
                .map(|head| WebHead {
                    id: head.id,
                    kind: head.kind,
                    origin: head.origin,
                    owned: head.owned,
                    orbit: head.orbit,
                })
                .collect(),
            devices: view
                .devices
                .into_iter()
                .map(|device| WebDevice {
                    id: device.id,
                    label: device.label,
                    state: device.state,
                    owned: device.owned,
                    degraded: device.degraded,
                    home: device.home,
                    pid: device.pid,
                    can_force_stop: device.can_force_stop,
                    last_error: device.last_error,
                })
                .collect(),
            storage: view
                .storage
                .into_iter()
                .map(|storage| WebStorage {
                    orbit: storage.orbit,
                    name: storage.name,
                    bytes_on_disk: storage.bytes_on_disk,
                    object_count: storage.object_count,
                    last_verified_ms: storage.last_verified_ms,
                    missing: storage.missing.map(missing_name),
                })
                .collect(),
            orbits: view
                .orbits
                .into_iter()
                .map(|orbit| WebOrbit {
                    space: orbit.space,
                    name: orbit.name,
                    path: orbit.path,
                    last_opened: orbit.last_opened,
                })
                .collect(),
            space: view.space.map(|space| WebSpace {
                space: space.space,
                whoami: space.whoami,
                admin: space.admin,
                members: space
                    .members
                    .into_iter()
                    .map(|member| WebMember {
                        id: member.id,
                        nick: member.nick,
                        authored_name: member.authored_name,
                        admin: member.admin,
                    })
                    .collect(),
                devices: space.devices,
                diagnosis: space.diagnosis.map(|diagnosis| WebDiagnosis {
                    gates: diagnosis
                        .gates
                        .into_iter()
                        .map(|gate| WebGate {
                            id: gate.id,
                            label: gate.label,
                            state: gate_state_name(gate.state),
                            detail: gate.detail,
                        })
                        .collect(),
                    blocked_on: diagnosis.blocked_on,
                    summary: diagnosis.summary,
                }),
            }),
            book: view.book.map(|book| WebBook {
                cards: book
                    .cards
                    .into_iter()
                    .map(|card| WebCard {
                        card: card.card,
                        name: card.name,
                        note: card.note,
                        handles: card.handles,
                        addresses: card.addresses,
                        devices: card.devices,
                        agents: card.agents,
                        picture: card.picture,
                        groups: card.groups,
                        self_claim: card.self_claim,
                        presence: card.presence.map(presence_name),
                    })
                    .collect(),
                migration_complete: book.migration_complete,
                migration_pending: book.migration_pending,
                migration_imported: book.migration_imported,
                suggestions: book
                    .suggestions
                    .into_iter()
                    .map(|suggestion| WebSuggestion {
                        suggestion: suggestion.suggestion,
                        name: suggestion.name,
                        note: suggestion.note,
                        handles: suggestion.handles,
                    })
                    .collect(),
            }),
            mcp: view.mcp.map(|mcp| WebMcpBinding {
                path: mcp.path,
                detail: mcp.detail,
                note: mcp.note,
                replaced: mcp.replaced,
                agent: mcp.agent,
                written: mcp.written,
                world: mcp.world,
            }),
            notices: view
                .notices
                .into_iter()
                .map(|notice| WebNotice {
                    said: notice.said,
                    launched: notice.launched,
                })
                .collect(),
            failures: view
                .failures
                .into_iter()
                .map(|failure| WebFailure {
                    what: failure.what,
                    error: failure.error,
                    retryable: failure.retryable,
                })
                .collect(),
            in_flight: view.in_flight,
        }
    }
}

fn presence_name(presence: api::PresenceView) -> &'static str {
    match presence {
        api::PresenceView::Online => "online",
        api::PresenceView::Away => "away",
        api::PresenceView::Offline => "offline",
    }
}

fn missing_name(missing: api::Missing) -> &'static str {
    match missing {
        api::Missing::NotPlaced => "notPlaced",
        api::Missing::Unreachable => "unreachable",
    }
}

fn gate_state_name(state: api::GateState) -> &'static str {
    match state {
        api::GateState::Pass => "pass",
        api::GateState::Wait => "wait",
        api::GateState::Fail => "fail",
        api::GateState::Warn => "warn",
        api::GateState::Skip => "skip",
    }
}

fn display_theme_name(theme: api::DisplayTheme) -> &'static str {
    match theme {
        api::DisplayTheme::Light => "light",
        api::DisplayTheme::Dark => "dark",
        api::DisplayTheme::HighContrast => "highContrast",
    }
}

fn display_sync_mode_name(mode: api::DisplaySyncMode) -> &'static str {
    match mode {
        api::DisplaySyncMode::StayInSync => "stayInSync",
        api::DisplaySyncMode::Positional => "positional",
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WebAction {
    Refresh,
    Open { entry_path: String },
    UpdateWorld { world: String },
    StartDevice { id: String },
    StopDevice { id: String },
    RestartDevice { id: String },
    ForceStopDevice { id: String },
    StopAllOwned,
    RemoveDevice { id: String, delete_data: bool },
    ReadSpace { orbit: String },
    StartHead,
    StopHead { id: String },
    ForgetOrbit { space: String },
    BookPut { card: Option<String>, name: String, note: Option<String> },
    BookDelete { card: String },
    BookSetPicture { card: String, path: Option<String> },
    BookMerge { from: String, into: String },
    BookClaimSelf { card: String },
    BookLink { card: String, handle: String },
    BookUnlink { card: String, handle: String },
    BookExport { path: String, cards: Option<Vec<String>> },
    BookImport { path: String },
    BookAccept { suggestion: String },
    BookDismiss { suggestion: String },
    InstallMcp {
        client: String,
        scope: Option<String>,
        name: String,
        agent: Option<String>,
        no_agent: bool,
        project: String,
        world: Option<String>,
        preview: bool,
    },
    DisplayPairingApprove { pairing: String, label: String },
    DisplayPairingReject { pairing: String },
    DisplayAssignmentPut {
        device: String,
        orbit: String,
        world: String,
        surface: String,
        input_json: String,
        theme: WebDisplayTheme,
        stale_after_ms: u32,
        on_stale: WebDisplayStaleAction,
        sync_group: Option<String>,
        sync_mode: WebDisplaySyncMode,
        static_delay_ms: i32,
        expires_at_unix_ms: Option<u64>,
    },
    DisplayAssignmentRevoke { assignment: String },
    DisplayDeviceRevoke { device: String },
}

/// The Flutter client owns exactly these two auxiliary top-level windows.
/// A request is a summon, never a navigation command: the existing window is
/// restored and focused rather than creating a second Book or Displays view.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum OwnedWindowSurface {
    Book,
    Displays,
}

impl OwnedWindowSurface {
    fn label(&self) -> &'static str {
        match self {
            Self::Book => "address-book",
            Self::Displays => "displays",
        }
    }

    fn query(&self) -> &'static str {
        match self {
            Self::Book => "book",
            Self::Displays => "displays",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Book => "Address book — Astrolabe",
            Self::Displays => "Displays — Astrolabe",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum WebDisplayTheme {
    Light,
    Dark,
    HighContrast,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum WebDisplayStaleAction {
    KeepWithNativeBanner,
    Blank,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum WebDisplaySyncMode {
    StayInSync,
    Positional,
}

impl From<WebAction> for ActionRequest {
    fn from(action: WebAction) -> Self {
        match action {
            WebAction::Refresh => Self::Refresh,
            WebAction::Open { entry_path } => Self::Open { entry_path },
            WebAction::UpdateWorld { world } => Self::UpdateWorld { world },
            WebAction::StartDevice { id } => Self::StartDevice { id },
            WebAction::StopDevice { id } => Self::StopDevice { id },
            WebAction::RestartDevice { id } => Self::RestartDevice { id },
            WebAction::ForceStopDevice { id } => Self::ForceStopDevice { id },
            WebAction::StopAllOwned => Self::StopAllOwned,
            WebAction::RemoveDevice { id, delete_data } => Self::RemoveDevice { id, delete_data },
            WebAction::ReadSpace { orbit } => Self::ReadSpace { orbit },
            WebAction::StartHead => Self::StartHead,
            WebAction::StopHead { id } => Self::StopHead { id },
            WebAction::ForgetOrbit { space } => Self::ForgetOrbit { space },
            WebAction::BookPut { card, name, note } => Self::BookPut { card, name, note },
            WebAction::BookDelete { card } => Self::BookDelete { card },
            WebAction::BookSetPicture { card, path } => Self::BookSetPicture { card, path },
            WebAction::BookMerge { from, into } => Self::BookMerge { from, into },
            WebAction::BookClaimSelf { card } => Self::BookClaimSelf { card },
            WebAction::BookLink { card, handle } => Self::BookLink { card, handle },
            WebAction::BookUnlink { card, handle } => Self::BookUnlink { card, handle },
            WebAction::BookExport { path, cards } => Self::BookExport { path, cards },
            WebAction::BookImport { path } => Self::BookImport { path },
            WebAction::BookAccept { suggestion } => Self::BookAccept { suggestion },
            WebAction::BookDismiss { suggestion } => Self::BookDismiss { suggestion },
            WebAction::InstallMcp { client, scope, name, agent, no_agent, project, world, preview } => {
                Self::InstallMcp { client, scope, name, agent, no_agent, project, world, preview }
            }
            WebAction::DisplayPairingApprove { pairing, label } => Self::DisplayPairingApprove { pairing, label },
            WebAction::DisplayPairingReject { pairing } => Self::DisplayPairingReject { pairing },
            WebAction::DisplayAssignmentPut {
                device,
                orbit,
                world,
                surface,
                input_json,
                theme,
                stale_after_ms,
                on_stale,
                sync_group,
                sync_mode,
                static_delay_ms,
                expires_at_unix_ms,
            } => Self::DisplayAssignmentPut {
                device,
                orbit,
                world,
                surface,
                input_json,
                theme: match theme {
                    WebDisplayTheme::Light => api::DisplayTheme::Light,
                    WebDisplayTheme::Dark => api::DisplayTheme::Dark,
                    WebDisplayTheme::HighContrast => api::DisplayTheme::HighContrast,
                },
                stale_after_ms,
                on_stale: match on_stale {
                    WebDisplayStaleAction::KeepWithNativeBanner => api::DisplayStaleAction::KeepWithNativeBanner,
                    WebDisplayStaleAction::Blank => api::DisplayStaleAction::Blank,
                },
                sync_group,
                sync_mode: match sync_mode {
                    WebDisplaySyncMode::StayInSync => api::DisplaySyncMode::StayInSync,
                    WebDisplaySyncMode::Positional => api::DisplaySyncMode::Positional,
                },
                static_delay_ms,
                expires_at_unix_ms,
            },
            WebAction::DisplayAssignmentRevoke { assignment } => Self::DisplayAssignmentRevoke { assignment },
            WebAction::DisplayDeviceRevoke { device } => Self::DisplayDeviceRevoke { device },
        }
    }
}

#[tauri::command]
fn client_current() -> WebClientView {
    api::current().into()
}

#[tauri::command]
fn client_dispatch(action: WebAction) -> WebClientView {
    api::dispatch(action.into()).into()
}

#[tauri::command]
async fn summon_owned_window(
    app: tauri::AppHandle,
    surface: OwnedWindowSurface,
) -> Result<(), String> {
    let label = surface.label();
    if let Some(window) = app.get_webview_window(label) {
        if window.is_minimized().map_err(|error| error.to_string())? {
            window.unminimize().map_err(|error| error.to_string())?;
        }
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }

    // `dev_url` keeps child windows on Vite's live document; packaged builds
    // use the app protocol and retain the same explicit route identity.
    let url = if let Some(mut dev_url) = app.config().build.dev_url.clone() {
        dev_url.set_query(Some(&format!("surface={}", surface.query())));
        WebviewUrl::External(dev_url)
    } else {
        WebviewUrl::CustomProtocol(
            format!("tauri://localhost/index.html?surface={}", surface.query())
                .parse()
                .map_err(|error| format!("invalid owned-window URL: {error}"))?,
        )
    };

    let builder = WebviewWindowBuilder::new(&app, label, url)
        .title(surface.title())
        .resizable(true)
        .minimizable(true)
        .visible(true);

    match surface {
        // Flutter's address-book host is permanently portrait: it can resize
        // within the rolodex range, but never maximise into a workspace.
        OwnedWindowSurface::Book => {
            builder
                .inner_size(370.0, 760.0)
                .min_inner_size(320.0, 600.0)
                .max_inner_size(440.0, 4096.0)
                .maximizable(false)
                .build()
                .map_err(|error| error.to_string())?;
        }
        // Displays is a resizable coordination workspace, matching Flutter's
        // 860×720 opening shape and 700×600 lower bound.
        OwnedWindowSurface::Displays => {
            builder
                .inner_size(860.0, 720.0)
                .min_inner_size(700.0, 600.0)
                .maximizable(true)
                .build()
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .append_invoke_initialization_script(PLATFORM_INIT)
        .setup(|app| {
            api::start(None, None).map_err(std::io::Error::other)?;
            let handle = app.handle().clone();
            api::subscribe(move |view| {
                let _ = handle.emit(CLIENT_VIEW_EVENT, WebClientView::from(view));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![client_current, client_dispatch, summon_owned_window])
        .run(tauri::generate_context!())
        .expect("run Astrolabe Web desktop host");
}
