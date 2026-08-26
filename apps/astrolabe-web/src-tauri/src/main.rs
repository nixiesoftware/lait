//! The desktop host is deliberately only an adapter.
//!
//! Astrolabe's Rust core retains the application model and action semantics.
//! This process starts that core, serializes the primary-window projection for
//! the WebView, and forwards its already-existing whole-view stream.

use astrolabe::api::{self, ActionRequest, ClientView, Staleness};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const CLIENT_VIEW_EVENT: &str = "astrolabe://client-view";
const MENU_EVENT: &str = "astrolabe://menu";
const EXIT_STAY_ID: &str = "exit-stay";
const EXIT_OFFLINE_ID: &str = "exit-offline";

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
    correspondence: Option<WebCorrespondenceFacts>,
    presentation: Option<WebPresentationFacts>,
    notices: Vec<WebNotice>,
    failures: Vec<WebFailure>,
    in_flight: Vec<String>,
    image: Option<WebImage>,
    update: Option<WebUpdate>,
    /// The host ends the process on seeing this; the page never draws it.
    exited: bool,
}

/// The one thing a person is ever asked about this client's own updating:
/// when to restart. Tagged, because the three cases are answered differently
/// and a surface that flattened them would have to reconstruct which it had.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WebUpdate {
    RestartRequested {
        version: String,
        urgency: &'static str,
    },
    Waiting {
        version: String,
        holding: Vec<String>,
    },
    Attention {
        why: String,
    },
}

/// The staged image this client spawns from, and whether the source was
/// rebuilt since — the fact behind the roll-forward affordance.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebImage {
    fingerprint: String,
    staged_at_ms: u64,
    source_changed: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebCorrespondenceFacts {
    my_device: Option<String>,
    /// What this identity hands somebody so they can reach it, rendered for
    /// copying. `None` until something has been published. Not a Card — that is
    /// the address book's and asserts nothing — and not an address, which is
    /// the directory's and is short and spoken.
    my_reach: Option<String>,
    /// Which conversation is this identity's own, when the backend has one.
    me: Option<String>,
    contacts: Vec<WebContact>,
    conversations: Vec<WebConversation>,
    open_tabs: Vec<String>,
    active_tab: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebContact {
    id: String,
    name: String,
    devices: Vec<String>,
    added: bool,
    is_agent: bool,
    parent_id: Option<String>,
    parent_name: Option<String>,
    unread: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebConversation {
    peer_id: String,
    peer_name: String,
    messages: Vec<WebChatMessage>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebChatMessage {
    mine: bool,
    kind: String,
    body: Option<String>,
    /// For an invitation, the link body it carries; `None` for a message.
    invitation: Option<String>,
    /// The deposit id for a received letter; `None` for one this identity sent.
    /// An invitation is acted on by naming this.
    id: Option<String>,
    sent_at: u64,
    from_device: String,
    provenance_agrees: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebPresentationFacts {
    chosen: Option<WebPresentationChoice>,
    program: Option<WebPresentedProgram>,
    failure: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebPresentationChoice {
    orbit: String,
    world: String,
    surface: String,
    title: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebPresentedProgram {
    assessment: String,
    partial_reasons: Vec<String>,
    cycle: String,
    refresh_after_ms: Option<u32>,
    items: Vec<WebPresentedItem>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebPresentedItem {
    id: String,
    duration_ms: Option<u32>,
    assessment: String,
    spoken_summary: Option<String>,
    scene: WebPresentedScene,
}

/// A frame crosses as a data URI rather than a byte array: the WebView draws
/// it straight into an `img` without a second encode on the JS side.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WebPresentedScene {
    Frame {
        uri: String,
        width: u32,
        height: u32,
    },
    Blank {
        reason: String,
    },
    Unsupported {
        output: String,
    },
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
    installed: bool,
    /// The channel this World follows by its own choice; `None` follows the
    /// node's.
    channel: Option<String>,
    /// The directory a local World is read from; `None` is a released World.
    source_dir: Option<String>,
    display_name: String,
    opens_at: Option<String>,
    version: Option<u32>,
    tagline: Option<String>,
    accent: Option<u32>,
    people: Option<Vec<WebWorldPerson>>,
    update: Option<WebWorldUpdate>,
    install: Option<WebWorldInstall>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebWorldInstall {
    phase: String,
    received: Option<u64>,
    total: Option<u64>,
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
    operation: Option<String>,
    phase: Option<String>,
    progress: Option<String>,
    message: Option<String>,
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
    /// The one World this head serves; `None` (a pre-pin head) matches no row.
    world: Option<String>,
    /// `running`, `exited` or `unknown`. Presence is not liveness: exited
    /// heads stay listed so a person can see the thing they opened died.
    state: String,
    state_detail: Option<String>,
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
    /// Hash of the image this device actually runs; compare against the
    /// view's `image.fingerprint` to spot a node on older code.
    image_fingerprint: Option<String>,
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
    /// `None` from a daemon that predates the custody split — not reported,
    /// as distinct from reported-as-none.
    identifier_custody: Option<WebIdentifierCustody>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebIdentifierCustody {
    /// Kinds of unlock path, never material.
    slots: Vec<String>,
    portable: bool,
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
        // Destructured whole, and deliberately without `..`.
        //
        // This is the seam where a field added to `api::ClientView` has to be
        // carried to the browser, and it is the only place that could say so.
        // Reading `field` — or destructuring with `..` — compiles exactly
        // as well when a field is missed, and the browser then draws a view
        // that quietly lacks it. That is the shape the api module's own doc
        // calls out: safe by construction rather than by care.
        //
        // So adding to the boundary fails *here*, until somebody decides what
        // this client does with it. Which is the decision, not the compile
        // error, and it should be made on purpose.
        let ClientView {
            loading,
            stale,
            library,
            host,
            display,
            presentation,
            heads,
            devices,
            storage,
            orbits,
            space,
            book,
            correspondence,
            notices,
            failures,
            in_flight,
            mcp,
            image,
            update,
            exited,
        } = view;
        Self {
            loading: loading,
            stale: stale.map(|stale| match stale {
                Staleness::NeverLoaded => WebStaleness {
                    kind: "neverLoaded",
                    reason: None,
                },
                Staleness::Signalled(reason) => WebStaleness {
                    kind: "signalled",
                    reason: Some(reason),
                },
            }),
            library: library.map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        // Destructured whole, without `..`, for the reason the
                        // row's own `update` already is and one level higher:
                        // the guarantee at the top of this impl stopped at
                        // `ClientView` and read fields from here, so a fact
                        // added to a World row compiled cleanly and never
                        // reached the browser. That is the same defect the
                        // nested comment below records having already had.
                        let api::LibraryRow {
                            key,
                            world_mount,
                            installed,
                            display_name,
                            opens_at,
                            version,
                            tagline,
                            accent,
                            people,
                            update,
                            install,
                            channel,
                            source_dir,
                        } = row;
                        WebLibraryWorld {
                            key,
                            world_mount,
                            installed,
                            display_name,
                            opens_at,
                            version,
                            tagline,
                            accent,
                            channel,
                            source_dir,
                            people: people.map(|people| {
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
                            update: update.map(|update| {
                                // Destructured whole for the same reason ClientView
                                // is: this row grew fields once and the browser
                                // quietly drew a view without them.
                                let api::WorldUpdateRow {
                                    serving,
                                    available,
                                    behind,
                                    unmet,
                                    operation,
                                    phase,
                                    progress,
                                    message,
                                } = update;
                                WebWorldUpdate {
                                    serving,
                                    available,
                                    behind,
                                    unmet,
                                    operation,
                                    phase,
                                    progress,
                                    message,
                                }
                            }),
                            install: install.map(|install| WebWorldInstall {
                                phase: install.phase,
                                received: install.received,
                                total: install.total,
                            }),
                        }
                    })
                    .collect()
            }),
            host: host.map(|host| WebHostFacts {
                version: host.version,
                identity_home: host.identity_home,
                spaces_root: host.spaces_root,
                orbit_count: host.orbit_count,
            }),
            display: display.map(|display| WebDisplayFacts {
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
                identifier_custody: display.identifier_custody.map(|custody| {
                    WebIdentifierCustody {
                        slots: custody.slots,
                        portable: custody.portable,
                    }
                }),
            }),
            heads: heads
                .into_iter()
                .map(|head| {
                    let api::HeadRow {
                        id,
                        kind,
                        orbit,
                        world,
                        origin,
                        owned,
                        state,
                        state_detail,
                    } = head;
                    WebHead {
                        id,
                        kind,
                        origin,
                        owned,
                        orbit,
                        world,
                        state,
                        state_detail,
                    }
                })
                .collect(),
            devices: devices
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
                    image_fingerprint: device.image_fingerprint,
                })
                .collect(),
            storage: storage
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
            orbits: orbits
                .into_iter()
                .map(|orbit| WebOrbit {
                    space: orbit.space,
                    name: orbit.name,
                    path: orbit.path,
                    last_opened: orbit.last_opened,
                })
                .collect(),
            space: space.map(|space| WebSpace {
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
            book: book.map(|book| WebBook {
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
            correspondence: correspondence.map(|corr| WebCorrespondenceFacts {
                my_reach: corr.my_reach.clone(),
                me: corr.me.clone(),
                my_device: corr.my_device,
                contacts: corr
                    .contacts
                    .into_iter()
                    .map(|contact| WebContact {
                        id: contact.id,
                        name: contact.name,
                        devices: contact.devices,
                        added: contact.added,
                        is_agent: contact.is_agent,
                        parent_id: contact.parent_id,
                        parent_name: contact.parent_name,
                        unread: contact.unread,
                    })
                    .collect(),
                conversations: corr
                    .conversations
                    .into_iter()
                    .map(|conversation| WebConversation {
                        peer_id: conversation.peer_id,
                        peer_name: conversation.peer_name,
                        messages: conversation
                            .messages
                            .into_iter()
                            .map(|message| WebChatMessage {
                                invitation: message.invitation.clone(),
                                id: message.id.clone(),
                                mine: message.mine,
                                kind: message.kind,
                                body: message.body,
                                sent_at: message.sent_at,
                                from_device: message.from_device,
                                provenance_agrees: message.provenance_agrees,
                            })
                            .collect(),
                    })
                    .collect(),
                open_tabs: corr.open_tabs,
                active_tab: corr.active_tab,
            }),
            presentation: presentation.map(|presentation| WebPresentationFacts {
                chosen: presentation.chosen.map(|chosen| WebPresentationChoice {
                    orbit: chosen.orbit,
                    world: chosen.world,
                    surface: chosen.surface,
                    title: chosen.title,
                }),
                program: presentation.program.map(|program| WebPresentedProgram {
                    assessment: program.assessment,
                    partial_reasons: program.partial_reasons,
                    cycle: program.cycle,
                    refresh_after_ms: program.refresh_after_ms,
                    items: program
                        .items
                        .into_iter()
                        .map(|item| WebPresentedItem {
                            id: item.id,
                            duration_ms: item.duration_ms,
                            assessment: item.assessment,
                            spoken_summary: item.spoken_summary,
                            scene: match item.scene {
                                api::PresentedScene::Frame {
                                    media_type,
                                    width,
                                    height,
                                    bytes,
                                } => WebPresentedScene::Frame {
                                    uri: format!(
                                        "data:image/{media_type};base64,{}",
                                        base64::engine::general_purpose::STANDARD.encode(bytes)
                                    ),
                                    width,
                                    height,
                                },
                                api::PresentedScene::Blank { reason } => {
                                    WebPresentedScene::Blank { reason }
                                }
                                api::PresentedScene::Unsupported { output } => {
                                    WebPresentedScene::Unsupported { output }
                                }
                            },
                        })
                        .collect(),
                }),
                failure: presentation.failure,
            }),
            mcp: mcp.map(|mcp| WebMcpBinding {
                path: mcp.path,
                detail: mcp.detail,
                note: mcp.note,
                replaced: mcp.replaced,
                agent: mcp.agent,
                written: mcp.written,
                world: mcp.world,
            }),
            notices: notices
                .into_iter()
                .map(|notice| WebNotice {
                    said: notice.said,
                    launched: notice.launched,
                })
                .collect(),
            failures: failures
                .into_iter()
                .map(|failure| WebFailure {
                    what: failure.what,
                    error: failure.error,
                    retryable: failure.retryable,
                })
                .collect(),
            in_flight: in_flight,
            image: image.map(|image| WebImage {
                fingerprint: image.fingerprint,
                staged_at_ms: image.staged_at_ms,
                source_changed: image.source_changed,
            }),
            exited,
            update: update.map(|update| match update {
                api::UpdateRow::RestartRequested { version, urgency } => {
                    WebUpdate::RestartRequested {
                        version,
                        urgency: match urgency {
                            api::UpdateUrgency::Quiet => "quiet",
                            api::UpdateUrgency::Insistent => "insistent",
                            api::UpdateUrgency::Urgent => "urgent",
                        },
                    }
                }
                api::UpdateRow::Waiting { version, holding } => {
                    WebUpdate::Waiting { version, holding }
                }
                api::UpdateRow::Attention { why } => WebUpdate::Attention { why },
            }),
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
// `rename_all` renames only the variants; the fields of struct variants keep
// their Rust names without `rename_all_fields`. The page sends camelCase for
// both, and the mismatch fails *nowhere* — the invoke rejects, the surface
// records a dispatch failure, and the action never reaches the core. The
// deserialization tests below hold this seam still.
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum WebAction {
    Refresh,
    /// End the client; goOffline stops what it owns first. Tray-dispatched.
    Exit {
        go_offline: bool,
    },
    /// Restage from the rebuilt source and bring everything back on the new
    /// image — the inner-loop roll-forward.
    Reload,
    OpenLink {
        url: String,
    },
    Open {
        world: String,
        entry_path: String,
    },
    UpdateWorld {
        world: String,
    },
    InstallWorld {
        world: String,
    },
    /// Follow a channel for this World alone, or (`None`) the node's.
    FollowWorldChannel {
        world: String,
        channel: Option<String>,
    },
    /// Register a tree on this device as a local World of its own. The
    /// directory is the whole ask; the name is derived from the tree.
    RegisterLocalWorld {
        dir: String,
    },
    /// Stop carrying a row for one. Nothing on disk is deleted.
    ForgetLocalWorld {
        key: String,
    },
    StartDevice {
        id: String,
    },
    StopDevice {
        id: String,
    },
    RestartDevice {
        id: String,
    },
    ForceStopDevice {
        id: String,
    },
    StopAllOwned,
    RemoveDevice {
        id: String,
        delete_data: bool,
    },
    ReadSpace {
        orbit: String,
    },
    StartHead,
    StopHead {
        id: String,
    },
    ForgetOrbit {
        space: String,
    },
    BookPut {
        card: Option<String>,
        name: String,
        note: Option<String>,
    },
    BookDelete {
        card: String,
    },
    BookSetPicture {
        card: String,
        path: Option<String>,
    },
    BookMerge {
        from: String,
        into: String,
    },
    BookClaimSelf {
        card: String,
    },
    BookLink {
        card: String,
        handle: String,
    },
    BookUnlink {
        card: String,
        handle: String,
    },
    BookExport {
        path: String,
        cards: Option<Vec<String>>,
    },
    BookImport {
        path: String,
    },
    BookAccept {
        suggestion: String,
    },
    BookDismiss {
        suggestion: String,
    },
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
    DisplayPairingApprove {
        pairing: String,
        label: String,
    },
    DisplayPairingReject {
        pairing: String,
    },
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
    DisplayAssignmentRevoke {
        assignment: String,
    },
    DisplayDeviceRevoke {
        device: String,
    },
    DisplayIdentifierAdmitPassphrase {
        passphrase: String,
    },
    SendMessage {
        to: String,
        body: String,
    },
    CollectMail,
    BlockSender {
        person: String,
    },
    AcceptContact {
        person: String,
    },
    /// Publish this identity's reach so it can be handed to somebody. Acts on
    /// nobody — showing a friend code is not befriending anyone.
    ShareReach,
    /// Take a correspondent in, by the announcement they handed over. The one
    /// of the pair that creates a relationship, which is why it is named for
    /// the person rather than for the artifact.
    AddCorrespondent {
        announcement: String,
    },
    /// Enter the Space an arriving invitation names. `message` is its deposit
    /// id; the coordinates verify against their own Space, so accepting is the
    /// same act as following an invite link — delivery was never admission.
    OpenInvitation {
        message: String,
    },
    /// Carry an invitation this identity already holds to a correspondent.
    /// Minting one is the Space's authority and stays there.
    SendInvitation {
        to: String,
        link: String,
    },
    OpenConversation {
        person: String,
    },
    FocusConversation {
        person: String,
    },
    CloseConversation {
        person: String,
    },
    EnterPresentation,
    PresentHere {
        orbit: String,
        world: String,
        surface: String,
        input: String,
        title: String,
    },
    PresentRefresh,
    LeavePresentation,
}

/// The Flutter client owns exactly these three auxiliary top-level windows.
/// A request is a summon, never a navigation command: the existing window is
/// restored and focused rather than creating a second view.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum OwnedWindowSurface {
    Book,
    Displays,
    Chat,
}

impl OwnedWindowSurface {
    fn label(&self) -> &'static str {
        match self {
            Self::Book => "address-book",
            Self::Displays => "displays",
            // Flutter's window key for the chat is `correspondence`.
            Self::Chat => "correspondence",
        }
    }

    fn query(&self) -> &'static str {
        match self {
            Self::Book => "book",
            Self::Displays => "displays",
            Self::Chat => "chat",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Book => "Address book — Astrolabe",
            Self::Displays => "Displays — Astrolabe",
            Self::Chat => "Chat — Astrolabe",
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
            WebAction::Exit { go_offline } => Self::Exit { go_offline },
            WebAction::Reload => Self::Reload,
            WebAction::OpenLink { url } => Self::OpenLink { url },
            WebAction::Open { world, entry_path } => Self::Open { world, entry_path },
            WebAction::UpdateWorld { world } => Self::UpdateWorld { world },
            WebAction::InstallWorld { world } => Self::InstallWorld { world },
            WebAction::FollowWorldChannel { world, channel } => {
                Self::FollowWorldChannel { world, channel }
            }
            WebAction::RegisterLocalWorld { dir } => Self::RegisterLocalWorld { dir },
            WebAction::ForgetLocalWorld { key } => Self::ForgetLocalWorld { key },
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
            WebAction::InstallMcp {
                client,
                scope,
                name,
                agent,
                no_agent,
                project,
                world,
                preview,
            } => Self::InstallMcp {
                client,
                scope,
                name,
                agent,
                no_agent,
                project,
                world,
                preview,
            },
            WebAction::DisplayPairingApprove { pairing, label } => {
                Self::DisplayPairingApprove { pairing, label }
            }
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
                    WebDisplayStaleAction::KeepWithNativeBanner => {
                        api::DisplayStaleAction::KeepWithNativeBanner
                    }
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
            WebAction::DisplayAssignmentRevoke { assignment } => {
                Self::DisplayAssignmentRevoke { assignment }
            }
            WebAction::DisplayDeviceRevoke { device } => Self::DisplayDeviceRevoke { device },
            WebAction::DisplayIdentifierAdmitPassphrase { passphrase } => {
                Self::DisplayIdentifierAdmitPassphrase { passphrase }
            }
            WebAction::SendMessage { to, body } => Self::SendMessage { to, body },
            WebAction::CollectMail => Self::CollectMail,
            WebAction::BlockSender { person } => Self::BlockSender { person },
            WebAction::AcceptContact { person } => Self::AcceptContact { person },
            WebAction::ShareReach => Self::ShareReach,
            WebAction::AddCorrespondent { announcement } => Self::AddCorrespondent { announcement },
            WebAction::OpenInvitation { message } => Self::OpenInvitation { message },
            WebAction::SendInvitation { to, link } => Self::SendInvitation { to, link },
            WebAction::OpenConversation { person } => Self::OpenConversation { person },
            WebAction::FocusConversation { person } => Self::FocusConversation { person },
            WebAction::CloseConversation { person } => Self::CloseConversation { person },
            WebAction::EnterPresentation => Self::EnterPresentation,
            WebAction::PresentHere {
                orbit,
                world,
                surface,
                input,
                title,
            } => Self::PresentHere {
                orbit,
                world,
                surface,
                input,
                title,
            },
            WebAction::PresentRefresh => Self::PresentRefresh,
            WebAction::LeavePresentation => Self::LeavePresentation,
        }
    }
}

/// Take the restart a staged release is waiting for.
///
/// A host capability rather than an `ActionRequest`, because the core cannot
/// do it: ending and relaunching this process is the host's own act, and the
/// core would have to ask the host anyway. It is also not an *update* —
/// nothing is applied here. Applying happens in the window this opens, at a
/// moment no client is alive: the stub's launch window on Windows and Linux,
/// the daemon's own act on macOS. All this does is reach that moment.
///
/// Never returns on success.
#[tauri::command]
fn restart_for_update(app: tauri::AppHandle, version: Option<String>) {
    // Under a stub the window is the stub's launch, not this process's own
    // relaunch — the stub is waiting on this very process.
    if astrolabe::client::update::request_relaunch(version.as_deref().unwrap_or("")) {
        app.exit(0);
    } else {
        app.restart();
    }
}

/// Big Picture takes the display, not the work area — and gives it back on
/// the way out, whatever the reason for leaving. Kept as a host command so
/// the WebView needs no window capability of its own.
#[tauri::command]
async fn set_fullscreen(window: tauri::WebviewWindow, fullscreen: bool) -> Result<(), String> {
    window
        .set_fullscreen(fullscreen)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn client_current() -> WebClientView {
    api::current().into()
}

/// The artwork one World declares, as data URIs. Not part of the view, and the
/// omission is the core's design: a surface asks only when the catalog or
/// selected release generation changes rather than remarshal images in each view.
/// An unknown mount answers with no artwork, not an error.
#[derive(Serialize)]
struct WebWorldArtwork {
    mark: Option<String>,
    hero: Option<String>,
}

#[tauri::command]
fn world_artwork(mount: String) -> WebWorldArtwork {
    let art = api::world_artwork(mount);
    let uri = |bytes: Option<Vec<u8>>| {
        bytes.map(|png| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )
        })
    };
    WebWorldArtwork {
        mark: uri(art.mark),
        hero: uri(art.hero),
    }
}

#[tauri::command]
fn client_dispatch(action: WebAction) -> WebClientView {
    api::dispatch(action.into()).into()
}

/// The per-World settings window. Unlike Book and Displays it is keyed per
/// World, and it receives a read-only snapshot in its URL rather than a
/// subscription: the settings page states what was true when it was opened,
/// exactly like the Flutter client's `--world-settings=` argv payload.
#[tauri::command]
async fn summon_world_settings(
    app: tauri::AppHandle,
    key: String,
    name: String,
    snapshot: String,
) -> Result<(), String> {
    // A Tauri window label admits [a-zA-Z0-9-/:_] only; a World key is not
    // bound by that alphabet, so anything else maps onto '-'.
    let sanitized: String = key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '/' | ':' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let label = format!("world-settings:{sanitized}");
    if let Some(window) = app.get_webview_window(&label) {
        if window.is_minimized().map_err(|error| error.to_string())? {
            window.unminimize().map_err(|error| error.to_string())?;
        }
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }

    // The snapshot is base64url and needs no further escaping in a query.
    let query = format!("surface=world-settings&snapshot={snapshot}");
    let url = if let Some(mut dev_url) = app.config().build.dev_url.clone() {
        dev_url.set_query(Some(&query));
        WebviewUrl::External(dev_url)
    } else {
        WebviewUrl::CustomProtocol(
            format!("tauri://localhost/index.html?{query}")
                .parse()
                .map_err(|error| format!("invalid settings-window URL: {error}"))?,
        )
    };

    // Flutter's settings window opens 560×680 and narrows to 440×520.
    let mut builder = WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("{name} settings — Astrolabe"))
        .resizable(true)
        .minimizable(true)
        .visible(true)
        .inner_size(560.0, 680.0)
        .min_inner_size(440.0, 520.0);
    builder = owned_by_main(&app, builder)?;
    builder.build().map_err(|error| error.to_string())?;
    Ok(())
}

/// Which Worlds draw the top rail themselves.
///
/// A World that answers here gets the window's whole height and the system
/// title bar goes transparent under it, so its own surface runs to the top
/// edge with the controls sitting in it. Every other World keeps the system
/// bar: a page that does not know the controls are over its top-left corner
/// would draw under them, and no page can find that out on its own.
#[cfg(target_os = "macos")]
fn draws_its_own_rail(world: &str) -> bool {
    world == "issues"
}

/// Where the controls land once the title bar is transparent, in CSS pixels
/// from the page's top-left corner. macOS keeps the buttons at a fixed offset
/// inside a 28pt bar and never tells the document about either, so the host
/// states the fact it owns and the World decides what to keep clear of.
#[cfg(target_os = "macos")]
const WINDOW_CONTROLS_INIT: &str = "window.__LAIT_WINDOW_CONTROLS__ = { top: 28, leading: 78 };";

/// The same fact, restated when it stops being true.
///
/// Full screen takes the controls away entirely, and a page still holding room
/// for them wears a band of nothing along its top edge — the defect this whole
/// change exists to remove, arriving by a different door. The host says so the
/// way it said it in the first place: it writes the fact and rings, and the
/// World decides again. One direction only — this is the host talking to a
/// page, never a page reaching the host, which is why `world:*` windows are
/// still absent from every capability.
#[cfg(target_os = "macos")]
fn restate_controls(overlapping: bool) -> String {
    let controls = if overlapping {
        "{ top: 28, leading: 78 }"
    } else {
        "null"
    };
    format!(
        "window.__LAIT_WINDOW_CONTROLS__ = {controls};\
         window.dispatchEvent(new Event('lait:window-controls'));"
    )
}

/// Not `owned_by_main`: a World keeps its own taskbar identity.
fn present_world_window(app: &tauri::AppHandle, launch: astrolabe::browser::WorldLaunch) {
    let sanitized: String = launch
        .world
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '/' | ':' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let label = format!("world:{sanitized}");
    let url: tauri::Url = match launch.url.parse() {
        Ok(url) => url,
        Err(_) => return,
    };
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.navigate(url);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url))
        .title(&launch.title)
        .inner_size(1280.0, 800.0)
        .min_inner_size(800.0, 600.0)
        .resizable(true)
        .maximizable(true)
        .minimizable(true)
        .visible(true);
    // The title stays on the window — the taskbar, the window menu and the
    // switcher all read it — it just stops being drawn over the page.
    #[cfg(target_os = "macos")]
    let builder = if draws_its_own_rail(&launch.world) {
        builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .initialization_script(WINDOW_CONTROLS_INIT)
    } else {
        builder
    };
    let built = builder.build();
    #[cfg(target_os = "macos")]
    if let Ok(window) = &built {
        if draws_its_own_rail(&launch.world) {
            // `Resized` rather than a fullscreen event, because there is no
            // fullscreen event to have: entering and leaving both arrive as a
            // resize, and the flag is only true afterwards. Latched, so an
            // ordinary drag-resize does not re-say a thing that has not
            // changed.
            let handle = window.clone();
            let overlapping = std::sync::atomic::AtomicBool::new(true);
            window.on_window_event(move |event| {
                if !matches!(event, tauri::WindowEvent::Resized(_)) {
                    return;
                }
                let now = !handle.is_fullscreen().unwrap_or(false);
                if now == overlapping.swap(now, std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let _ = handle.eval(restate_controls(now));
            });
        }
    }
    if built.is_err() {
        let _ = astrolabe::browser::open(&launch.url);
    }
}

/// Owner on Windows, child on macOS, transient-for on Linux. A missing main
/// window degrades to an unowned one rather than refusing to open.
fn owned_by_main<'a>(
    app: &'a tauri::AppHandle,
    builder: WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle>,
) -> Result<WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle>, String> {
    match app.get_webview_window("main") {
        Some(main) => builder.parent(&main).map_err(|error| error.to_string()),
        None => Ok(builder),
    }
}

#[tauri::command]
async fn summon_owned_window(
    app: tauri::AppHandle,
    surface: OwnedWindowSurface,
) -> Result<(), String> {
    summon_surface(&app, surface)
}

fn summon_surface(app: &tauri::AppHandle, surface: OwnedWindowSurface) -> Result<(), String> {
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

    let builder = owned_by_main(
        app,
        WebviewWindowBuilder::new(app, label, url)
            .title(surface.title())
            .resizable(true)
            .minimizable(true)
            .visible(true),
    )?;

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
        // The chat opens at Flutter's 760×660 and narrows to 520×480.
        OwnedWindowSurface::Chat => {
            builder
                .inner_size(760.0, 660.0)
                .min_inner_size(520.0, 480.0)
                .maximizable(true)
                .build()
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// The tray: the client's standing presence while every window is closed.
/// Closing is not stopping — the primary window hides here so its Spaces keep
/// converging, and this is the one place that genuinely quits.
fn install_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::TrayIconBuilder;

    // The two answers of `lifecycle::ExitRequest`, never one generic Quit.
    let open = MenuItem::with_id(app, "open", "Open Astrolabe", true, None::<&str>)?;
    let stay = MenuItem::with_id(
        app,
        EXIT_STAY_ID,
        "Close and stay online",
        true,
        None::<&str>,
    )?;
    let offline = MenuItem::with_id(
        app,
        EXIT_OFFLINE_ID,
        "Go offline and exit",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(
        app,
        &[&open, &PredefinedMenuItem::separator(app)?, &stay, &offline],
    )?;

    let mut tray = TrayIconBuilder::with_id("astrolabe")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Astrolabe")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            // The process ends on the view's `exited` cue, never here: the
            // shell must not close before the stopping has happened.
            id if dispatch_menu_exit(id) => {}
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn menu_exit_policy(id: &str) -> Option<bool> {
    match id {
        EXIT_STAY_ID => Some(false),
        EXIT_OFFLINE_ID => Some(true),
        _ => None,
    }
}

fn dispatch_menu_exit(id: &str) -> bool {
    let Some(go_offline) = menu_exit_policy(id) else {
        return false;
    };
    let _ = api::dispatch(ActionRequest::Exit { go_offline });
    true
}

/// The application menu, where the operating system keeps one of its own.
/// macOS gives every application a menu bar above its windows; declaring it
/// replaces the default whole, so the standard application items are declared
/// too. Everywhere else the wordmark in the caption is the only application
/// menu there is, and this does nothing.
#[cfg(target_os = "macos")]
fn install_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};

    // macOS's predefined Quit exits immediately inside the native menu
    // implementation, bypassing the client's asynchronous lifecycle policy.
    // These are ordinary menu events, and the two labels make the policy an
    // explicit choice just as the tray does on every platform.
    let stay = MenuItemBuilder::with_id(EXIT_STAY_ID, "Close and stay online").build(app)?;
    let offline = MenuItemBuilder::with_id(EXIT_OFFLINE_ID, "Go offline and exit").build(app)?;
    let application = SubmenuBuilder::new(app, "Astrolabe")
        .about(None)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .item(&stay)
        .item(&offline)
        .build()?;

    // ⌘R is what a Mac reaches for; F5 still works in the page itself.
    let refresh = MenuItemBuilder::with_id("refresh", "Refresh local state")
        .accelerator("Cmd+R")
        .build(app)?;
    let theme = MenuItemBuilder::with_id("theme", "Toggle theme").build(app)?;
    let client = SubmenuBuilder::new(app, "Client")
        .item(&refresh)
        .item(&theme)
        .build()?;

    let displays = MenuItemBuilder::with_id("displays", "Displays")
        .accelerator("Cmd+Shift+D")
        .build(app)?;
    let book = MenuItemBuilder::with_id("book", "Address book")
        .accelerator("Cmd+Shift+B")
        .build(app)?;
    let chat = MenuItemBuilder::with_id("chat", "Chat")
        .accelerator("Cmd+Shift+M")
        .build(app)?;
    let window = SubmenuBuilder::new(app, "Window")
        .item(&displays)
        .item(&book)
        .item(&chat)
        .separator()
        .minimize()
        .fullscreen()
        .build()?;

    let menu = MenuBuilder::new(app)
        .item(&application)
        .item(&client)
        .item(&window)
        .build()?;
    app.set_menu(menu)?;

    app.on_menu_event(|app, event| match event.id.as_ref() {
        // Refresh and theme act on the model, which lives behind the main
        // window's page: forwarded as an event rather than re-implemented.
        "refresh" | "theme" => {
            let _ = app.emit(MENU_EVENT, event.id.as_ref().to_string());
        }
        "displays" => {
            let _ = summon_surface(app, OwnedWindowSurface::Displays);
        }
        "book" => {
            let _ = summon_surface(app, OwnedWindowSurface::Book);
        }
        "chat" => {
            let _ = summon_surface(app, OwnedWindowSurface::Chat);
        }
        id if dispatch_menu_exit(id) => {}
        _ => {}
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page speaks camelCase for variants *and* fields. `rename_all`
    /// covers only the variants, so the field half rests on
    /// `rename_all_fields` — and losing it fails nowhere at build time: the
    /// invoke rejects, the surface records "Dispatch open failed", and LAUNCH
    /// reads as a control that does nothing. These are the exact payloads the
    /// page sends.
    #[test]
    fn the_actions_the_page_sends_deserialize_whole() {
        let payloads = [
            r#"{"type":"reload"}"#,
            r#"{"type":"exit","goOffline":true}"#,
            r#"{"type":"displayIdentifierAdmitPassphrase","passphrase":"a dozen letters"}"#,
            r#"{"type":"openLink","url":"lait://world/issues"}"#,
            r#"{"type":"open","world":"issues","entryPath":"/"}"#,
            r#"{"type":"updateWorld","world":"issues"}"#,
            r#"{"type":"removeDevice","id":"dev","deleteData":true}"#,
            r#"{"type":"installMcp","client":"claude","scope":null,"name":"lait","agent":null,"noAgent":false,"project":".","world":null,"preview":true}"#,
            r#"{"type":"displayAssignmentPut","device":"d","orbit":"o","world":"w","surface":"s","inputJson":"{}","theme":"dark","staleAfterMs":1000,"onStale":"blank","syncGroup":null,"syncMode":"positional","staticDelayMs":0,"expiresAtUnixMs":null}"#,
        ];
        for payload in payloads {
            if let Err(error) = serde_json::from_str::<WebAction>(payload) {
                panic!("the page's own payload was refused: {error}\n  {payload}");
            }
        }
    }

    #[test]
    fn every_menu_exit_is_one_of_the_two_explicit_lifecycle_policies() {
        assert_eq!(menu_exit_policy(EXIT_STAY_ID), Some(false));
        assert_eq!(menu_exit_policy(EXIT_OFFLINE_ID), Some(true));
        assert_eq!(menu_exit_policy("quit"), None);

        // The native predefined item performs its own immediate exit. It must
        // not return to the macOS application menu, where it would bypass the
        // policy mapping above.
        let source = include_str!("main.rs");
        let predefined_quit = [".qu", "it()"].concat();
        assert!(
            !source.contains(&predefined_quit),
            "the macOS menu contains a one-step native Quit"
        );
    }
}

fn main() {
    // Before any window exists: a later launch hands its arguments — a
    // `lait:` link among them — to the running client and ends here. On a
    // claim that errs, api::start's backstop still guards the state root.
    match api::claim_single_instance() {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => eprintln!("astrolabe: {error}"),
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .append_invoke_initialization_script(PLATFORM_INIT)
        .setup(|app| {
            astrolabe::client::update::identify_running_version(
                app.package_info().version.to_string(),
            );
            let resources = app.path().resource_dir().map_err(std::io::Error::other)?;
            let mut catalog = resources.join("world-catalog");
            // The canonical Linux package is a relocatable stable-root tree,
            // not a system package under /usr/lib. Tauri therefore resolves
            // its conventional resource directory outside that tree. The
            // release builder carries the same catalog resource beside the
            // host on Windows and Linux; prefer Tauri's platform location and
            // fall back to that owned sibling only when it is the one present.
            if !catalog.is_dir() {
                if let Some(beside) = std::env::current_exe()
                    .map_err(std::io::Error::other)?
                    .parent()
                    .map(|parent| parent.join("world-catalog"))
                    .filter(|candidate| candidate.is_dir())
                {
                    catalog = beside;
                }
            }
            api::start_with_catalog(None, None, Some(catalog.to_string_lossy().into_owned()))
                .map_err(std::io::Error::other)?;
            // Window creation hops to the main thread; every platform makes
            // windows there.
            let presenter = app.handle().clone();
            astrolabe::browser::present_with(move |launch| {
                let handle = presenter.clone();
                let _ = presenter.run_on_main_thread(move || present_world_window(&handle, launch));
            });
            let handle = app.handle().clone();
            api::subscribe(move |view| {
                let exited = view.exited;
                let _ = handle.emit(CLIENT_VIEW_EVENT, WebClientView::from(view));
                if exited {
                    handle.exit(0);
                }
            });
            let summoner = app.handle().clone();
            api::on_second_launch(move || {
                let handle = summoner.clone();
                let _ = handle.clone().run_on_main_thread(move || {
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.unminimize();
                        let _ = window.set_focus();
                    }
                });
            });
            install_tray(app.handle())?;
            #[cfg(target_os = "macos")]
            install_menu(app.handle())?;
            // The launch-argument half: Windows and Linux hand a registered
            // scheme's URL to a fresh process this way, and so does a first
            // launch on macOS.
            if let Some(link) = astrolabe::link::Link::from_args(std::env::args()) {
                open_link(&link.to_url());
            }
            Ok(())
        })
        // Closing the primary window is not stopping: it hides to the tray so
        // this identity's Spaces keep converging. Secondary windows really
        // close — they are views, not the client.
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            client_current,
            client_dispatch,
            world_artwork,
            set_fullscreen,
            restart_for_update,
            summon_owned_window,
            summon_world_settings
        ])
        .build(tauri::generate_context!())
        .expect("build Astrolabe Web desktop host")
        // A `lait:` link arrives two ways and both are the OS handing this
        // process a URL: as a launch argument, and — while already running,
        // which is the macOS path — as an open event.
        .run(|_app, _event| {
            // `RunEvent::Opened` exists only on macOS and iOS; on the stub
            // platforms a link arrives as a launch argument instead.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = _event {
                for url in urls {
                    open_link(url.as_str());
                }
            }
        });
}

/// Hand one `lait:` URL to the core, which decides what it names and whether
/// this build can act on it. Refusals surface in the view like any other.
fn open_link(url: &str) {
    let _ = api::dispatch(ActionRequest::OpenLink {
        url: url.to_owned(),
    });
}
