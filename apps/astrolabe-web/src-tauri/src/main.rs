//! The desktop host is deliberately only an adapter.
//!
//! Astrolabe's Rust core retains the application model and action semantics.
//! This process starts that core, serializes the primary-window projection for
//! the WebView, and forwards its already-existing whole-view stream.

use astrolabe::api::{self, ActionRequest, ClientView, Staleness};
use serde::{Deserialize, Serialize};
use tauri::Emitter;

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
    heads: Vec<WebHead>,
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WebAction {
    Refresh,
    Open { entry_path: String },
    UpdateWorld { world: String },
    StopHead { id: String },
}

impl From<WebAction> for ActionRequest {
    fn from(action: WebAction) -> Self {
        match action {
            WebAction::Refresh => Self::Refresh,
            WebAction::Open { entry_path } => Self::Open { entry_path },
            WebAction::UpdateWorld { world } => Self::UpdateWorld { world },
            WebAction::StopHead { id } => Self::StopHead { id },
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
        .invoke_handler(tauri::generate_handler![client_current, client_dispatch])
        .run(tauri::generate_context!())
        .expect("run Astrolabe Web desktop host");
}
