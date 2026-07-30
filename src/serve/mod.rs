//! `lait serve` — the local HTTP surface, and the browser's Layer-B client.
//!
//! The daemon's contract has always been [`crate::control`]: a versioned,
//! hand-maintained imperative façade over the CRDT, spoken over a Unix socket or
//! a named pipe. Native clients are local processes, so that
//! transport cost them nothing. A browser cannot speak a named pipe. This module
//! is the *one* adapter that closes that gap — the same `Request`/`Response`
//! types, the same `Doorbell` stream, re-bound to a loopback TCP socket and SSE.
//!
//! Two things follow, and they are the whole design:
//!
//! **This is a client of the host plane.** The browser is a picker over all
//! registered Orbits. [`crate::daemon::OrbitDirectory`] supplies passive
//! discovery; the identity-scoped Lait daemon owns lazy Station placement,
//! routing, and doorbell fan-in.
//!
//! **The socket was the authentication.** Binding the same façade to a TCP port
//! removes the OS permission check that made auth unnecessary, and adds a caller
//! that never existed before: the web pages the user visits. See [`auth`].
//!
//! The browser is deliberately *not* a peer. It holds no key, has no entry in the
//! ACL, and is never invited: it is a lens on a device's replica, and the device
//! remains the only network identity.

pub mod auth;
mod bridge;
mod content;
pub mod policy;
pub mod spaces;

mod shell;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Redirect, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use tokio::net::TcpListener;

use crate::control::{ErrorKind, Request};
use crate::daemon::{LaitDaemonClient, OrbitDirectory, OrbitDoorbell, StationIdentity};
use auth::{Guard, Refusal};

/// The default port. Fixed rather than ephemeral so the URL is predictable and
/// the `Origin` allowlist has something stable to name; a collision is reported
/// rather than silently worked around, because a `lait serve` that lands on a
/// *different* port than it was asked for is a footgun for anything that
/// bookmarked it.
pub const DEFAULT_PORT: u16 = 7717;

/// The cookie the browser trades its one-time URL token for.
///
/// Named per-port, because **cookies ignore the port**: `127.0.0.1:7717` and
/// `127.0.0.1:7801` are the same cookie origin, so a fixed name would have two
/// concurrent `lait serve` runs silently clobbering each other's credential —
/// whichever loaded last wins, and the other tab starts 401ing. The port is not a
/// security boundary here (the token is); it is what keeps two runs from being the
/// same jar entry.
fn cookie_name(port: u16) -> String {
    format!("lait_token_{port}")
}

struct App {
    guard: Guard,
    directory: OrbitDirectory,
    daemon: LaitDaemonClient,
    doorbells: tokio::sync::broadcast::Sender<ViewerEvent>,
    cookie: String,
    /// Latched when this server begins shutting down.
    ///
    /// On `App` rather than passed down, because every long-lived response has
    /// to see it and they are constructed by handlers that share nothing else.
    stop: tokio::sync::watch::Sender<bool>,
    /// How many content transfers may be in flight at once.
    content_permits: content::ContentStreamPermits,
    /// The bridge's own fan-out. Deliberately not `doorbells`: one ring for what
    /// every tab must see, one for what only the tab that started a transfer
    /// cares about.
    bridge: bridge::BridgeHub,
}

#[derive(Clone)]
enum ViewerEvent {
    Doorbell(OrbitDoorbell),
    Lagged,
}

/// Run the local server until interrupted.
///
/// `json` swaps the human sentence for a one-line object carrying the same facts:
///
/// ```json
/// {"url":"http://127.0.0.1:7717/?token=…","token":"…","port":7717}
/// ```
///
/// It exists because tooling needs the token and the only alternative was scraping
/// it out of a sentence with a regex — which makes prose written for a human into an
/// API, so improving the wording becomes a breaking change. `viewer/scripts/dev.mjs`
/// is the first caller; an editor plugin that wants to embed the client is the next.
/// The line is emitted **before** the server starts accepting, so a parent process
/// can read one line and know it is safe to connect.
pub async fn run(port: u16, open: bool, json: bool) -> Result<()> {
    // Identity scoping, resolved once at startup. OrbitDirectory uses
    // `$LAIT_HOME` as the self-contained identity boundary.
    let identity = crate::config::identity_dir()?;
    let self_contained = std::env::var_os("LAIT_HOME").is_some();
    let agents_base = crate::registry::agents_base(&crate::config::config_root()?);

    // Loopback only. Not `0.0.0.0`: that would hand the LAN an unauthenticated-
    // by-default view of every space on this machine, and the token is the only
    // thing that would stand between them and it.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .with_context(|| {
            format!("bind 127.0.0.1:{port} (is another `lait serve` already running?)")
        })?;
    let bound = listener.local_addr().context("read bound address")?;

    let token = mint_token();
    crate::cli::ensure_lait_daemon()
        .await
        .context("start Lait daemon")?;
    let daemon = LaitDaemonClient::current()?;
    let (doorbells, _) = tokio::sync::broadcast::channel(256);
    // Created before anything that has to watch it. Every long-lived response
    // and every background task selects on this one channel.
    let (stop, _) = tokio::sync::watch::channel(false);
    let app = Arc::new(App {
        guard: Guard::new(token.clone(), bound.port()),
        directory: OrbitDirectory::new(identity, agents_base, self_contained),
        daemon: daemon.clone(),
        doorbells: doorbells.clone(),
        cookie: cookie_name(bound.port()),
        stop: stop.clone(),
        content_permits: content::ContentStreamPermits::new(),
        bridge: bridge::BridgeHub::new(),
    });
    // The signal is fired before `shutdown_signal()` reaches axum, not after.
    // Graceful shutdown waits on in-flight responses, and this server has two
    // that never complete on their own: an SSE stream whose item type is
    // `Infallible` and whose broadcast receiver only ends when every sender
    // drops, and (from the bridge onward) a WebSocket. Handing axum a shutdown
    // future while those are open is a wait with no end, so the signal has to
    // reach *them* first.
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(pump_daemon_events(daemon, doorbells, stop.subscribe()));

    let url = format!("http://127.0.0.1:{}/?token={}", bound.port(), token);
    if json {
        // One line, then keep serving — the same shape `watch` has: a long-running
        // command whose first output is the fact you were waiting for. Rust's
        // stdout is a `LineWriter`, so the newline flushes it to a piped parent
        // without an explicit flush.
        println!(
            "{}",
            serde_json::json!({ "url": url, "token": token, "port": bound.port() })
        );
    } else {
        println!("lait serve — your spaces at:\n  {url}");
        println!("(loopback only; this link carries a one-time token for this run)");
    }
    if open {
        open_browser(&url);
    }

    let serve_result = axum::serve(listener, router(app))
        .with_graceful_shutdown({
            let stop = stop.clone();
            async move {
                shutdown_signal().await;
                // Every long-lived task selects on this. Sending before axum
                // begins draining is what lets the drain finish at all.
                let _ = stop.send(true);
            }
        })
        .await
        .context("serve");
    // Joined rather than aborted. An abort drops a task at whatever await it
    // happened to be sitting on, which for the pump is in the middle of a
    // daemon subscription; joining after the signal lets it return on its own
    // and makes "did it stop" a fact rather than a hope.
    let _ = stop.send(true);
    tasks.shutdown().await;
    serve_result
}

/// The shortest and longest waits between reconnection attempts.
///
/// Bounded at both ends. Half a second is fast enough that a daemon restart
/// looks instant; thirty seconds is where an unreachable daemon stops costing
/// anything. The old fixed half-second was the problem: an unreachable daemon
/// emitted `Lagged` twice a second forever, and every one of those costs the
/// viewer a full rebaseline.
const PUMP_BACKOFF_FLOOR: std::time::Duration = std::time::Duration::from_millis(500);
const PUMP_BACKOFF_CEILING: std::time::Duration = std::time::Duration::from_secs(30);

async fn pump_daemon_events(
    daemon: LaitDaemonClient,
    doorbells: tokio::sync::broadcast::Sender<ViewerEvent>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut backoff = PUMP_BACKOFF_FLOOR;
    // A run of failures is one event, not one per attempt. The viewer's response
    // to `Lagged` is to re-read everything, so repeating it while the daemon is
    // still down is a rebaseline storm that tells the user nothing new. The flag
    // clears on the first success, so the *next* outage is announced again.
    let mut announced = false;
    loop {
        if *stop.borrow() {
            return;
        }
        match daemon.subscribe_catalog().await {
            Ok(mut subscription) => {
                backoff = PUMP_BACKOFF_FLOOR;
                announced = false;
                loop {
                    let next = tokio::select! {
                        biased;
                        _ = stop.changed() => return,
                        next = subscription.next() => next,
                    };
                    match next {
                        Ok(Some(doorbell)) => {
                            let _ = doorbells.send(ViewerEvent::Doorbell(doorbell));
                        }
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(%error, "Lait daemon event stream ended");
                            break;
                        }
                    }
                }
                // The stream ending IS news the viewer has to act on, whatever
                // happens next: frames were missed, so its projection is stale.
                let _ = doorbells.send(ViewerEvent::Lagged);
            }
            Err(error) => {
                tracing::debug!(%error, "Lait daemon event endpoint is unavailable");
                if !announced {
                    announced = true;
                    let _ = doorbells.send(ViewerEvent::Lagged);
                }
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(PUMP_BACKOFF_CEILING);
    }
}

/// Stop the viewer adapter without stopping the independently owned Lait daemon.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    {
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler")
                .recv()
                .await;
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}

/// Every path this server answers, and how to reach it.
///
/// A list rather than a sequence of `.route` calls, because the property that
/// matters is not "these paths exist" but "no path escapes the gate", and
/// `Router::layer` wraps only what precedes it. A route registered after the
/// layer ships with no origin check and no token check, and nothing about the
/// code looks wrong.
///
/// Sourcing the router from this list is what makes the guarantee checkable: a
/// test can walk the same list, and a route added anywhere else is a route the
/// test never sees — which is the failure it is supposed to catch.
const ROUTES: &[Route] = &[
    Route::open("/", Method::Get),
    Route::open("/api/spaces", Method::Get),
    Route::open("/api/spaces/{id}/rpc", Method::Post),
    Route::open("/api/spaces/{id}/worlds/{world}/rpc", Method::Post),
    Route::open("/api/events", Method::Get),
    Route::no_query_token("/api/spaces/{id}/content", Method::Post),
    Route::no_query_token("/api/spaces/{id}/content/{content}", Method::Get),
    Route::no_query_token("/api/spaces/{id}/content/{content}", Method::Head),
    // The upgrade takes a cookie or a Bearer header and never a query token. A
    // browser's `WebSocket` constructor cannot set a header, so in practice this
    // is the cookie — which is the credential that already rides a same-origin
    // handshake, and the one that does not end up in history.
    Route::no_query_token("/api/session", Method::Get),
];

/// One registered path: how to reach it, and whether it will take a credential
/// out of the query string.
struct Route {
    path: &'static str,
    method: Method,
    /// Whether `?token=` is accepted here.
    ///
    /// It is, on `/`, and that is deliberate — the opening navigation carries
    /// the token in the URL and `index` immediately trades it for a cookie and
    /// redirects, so it never lingers. Every other path had no reason to accept
    /// it and no reason to refuse it either, until content: a download URL is a
    /// thing people paste, put in a `src`, and leave in their history. A live
    /// credential in one of those is a credential in the URL bar, in devtools,
    /// in the download list, and in whatever logs the dev proxy keeps.
    ///
    /// So the refusal is a property of the route, decided once, rather than a
    /// branch each new handler remembers to write.
    query_token: bool,
}

impl Route {
    const fn open(path: &'static str, method: Method) -> Self {
        Self {
            path,
            method,
            query_token: true,
        }
    }
    const fn no_query_token(path: &'static str, method: Method) -> Self {
        Self {
            path,
            method,
            query_token: false,
        }
    }
}

/// Which verb a registered path answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    Get,
    Post,
    Head,
}

fn handler(route: &Route) -> axum::routing::MethodRouter<Arc<App>> {
    match (route.path, route.method) {
        ("/", _) => get(index),
        ("/api/spaces", _) => get(list_spaces),
        ("/api/spaces/{id}/rpc", _) => post(rpc),
        ("/api/spaces/{id}/worlds/{world}/rpc", _) => post(world_rpc),
        ("/api/events", _) => get(events),
        ("/api/spaces/{id}/content", _) => post(content::upload),
        ("/api/spaces/{id}/content/{content}", Method::Head) => axum::routing::head(content::head),
        ("/api/spaces/{id}/content/{content}", _) => get(content::download),
        ("/api/session", _) => get(bridge::session),
        (other, _) => unreachable!("{other} is in ROUTES with no handler"),
    }
}

fn router(app: Arc<App>) -> Router {
    let mut router = Router::new();
    for route in ROUTES {
        router = router.route(route.path, handler(route));
    }
    router
        // Everything else is the client: a real asset, or the SPA entry so the
        // app can resolve its own routes. Registered last so it can never shadow
        // `/api`, and inside the gate like everything else — the bundle is not
        // secret, but an unauthenticated page that immediately 401s on every
        // fetch is a worse experience than an honest refusal.
        .fallback(get(static_asset))
        .layer(axum::middleware::from_fn_with_state(app.clone(), gate))
        .with_state(app)
}

/// A 32-byte hex token, minted per run and never persisted.
fn mint_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("getrandom");
    data_encoding::HEXLOWER.encode(&buf)
}

/// The gate every request passes: rebinding guard first, credential second.
///
/// Ordering is deliberate. `check_origin` is what survives a successful rebind
/// (at which point the browser *will* hand over our cookie), so it must not be
/// reachable-past by anything the attacker controls. The token is checked only
/// once we already believe the request is addressed to us by a loopback name.
async fn gate(State(app): State<Arc<App>>, req: axum::extract::Request, next: Next) -> Response {
    let headers = req.headers();
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if let Err(r) = app.guard.check_origin(host, origin) {
        return refuse(r);
    }

    // Three ways to present the token, one meaning. The query form exists only
    // for the opening navigation — `index` immediately trades it for the cookie
    // and redirects, so it never lingers in history or a Referer.
    //
    // Precedence is load-bearing: **query beats cookie**. The token is per-run,
    // but the cookie outlives the run that set it, so after a restart the jar
    // holds a stale credential. Consulting it first would shadow the fresh token
    // the user was just handed and 401 them out of the link they legitimately
    // clicked — with no way back, since nothing in the UI can clear a cookie it
    // cannot read. An explicit token in the URL is a deliberate handoff and wins.
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| auth::cookie_value(c, &app.cookie));
    let query = req.uri().query().and_then(|q| query_param(q, "token"));

    // Whether the query form counts here is a property of the path, read from
    // the same table the router is built from. Matched against the raw path,
    // because the gate runs before routing resolves parameters — so the
    // comparison is structural, not string equality.
    let query = accepts_query_token(req.uri().path())
        .then_some(query)
        .flatten();

    if let Err(r) = app
        .guard
        .check_token(resolve_token(bearer, query.as_deref(), cookie))
    {
        return refuse(r);
    }
    next.run(req).await
}

/// Whether this concrete path is allowed to present its credential in the query
/// string.
///
/// Unknown paths default to refusing it. The fallback serves the bundle, which
/// a browser reaches with a cookie it already has; a path nobody registered has
/// no business carrying a token in a URL either way, and defaulting the other
/// direction would make every future route opt *out* of the risk.
fn accepts_query_token(path: &str) -> bool {
    ROUTES
        .iter()
        .find(|route| path_matches(route.path, path))
        .is_some_and(|route| route.query_token)
}

/// Whether a concrete request path matches a registered pattern.
///
/// `{name}` matches exactly one segment. Deliberately not a general router: the
/// question here is only "which registered route is this", asked before axum
/// answers it, and a second full matcher would be a second thing to keep in
/// step with the first.
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.split('/');
    let mut path = path.split('/');
    loop {
        match (pattern.next(), path.next()) {
            (None, None) => return true,
            (Some(p), Some(c)) if p.starts_with('{') && p.ends_with('}') => {
                if c.is_empty() {
                    return false;
                }
            }
            (Some(p), Some(c)) if p == c => {}
            _ => return false,
        }
    }
}

/// Which presented credential wins.
///
/// Extracted so the test and the gate exercise the *same* order. Inlined, the
/// precedence could only be tested by a copy of it — which stays green when the
/// real one is reordered, i.e. exactly when the regression it guards happens.
fn resolve_token<'a>(
    bearer: Option<&'a str>,
    query: Option<&'a str>,
    cookie: Option<&'a str>,
) -> Option<&'a str> {
    bearer.or(query).or(cookie)
}

fn refuse(r: Refusal) -> Response {
    let code = match r {
        Refusal::BadToken => StatusCode::UNAUTHORIZED,
        _ => StatusCode::FORBIDDEN,
    };
    (code, err_json(r.reason(), ErrorKind::Error)).into_response()
}

/// Errors go out in the same envelope `--json` emits, so a browser client and a
/// CLI client are reading one contract rather than two.
fn err_json(message: &str, error_kind: ErrorKind) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "kind": "error",
        "message": message,
        "error_kind": error_kind,
    }))
}

/// Minimal `application/x-www-form-urlencoded` lookup — one key, no allocation
/// beyond the hit. Avoids a query-string crate for a single parameter.
fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

#[derive(Deserialize)]
struct IndexQuery {
    token: Option<String>,
}

/// The shell — and the one-time token handoff.
///
/// Arriving with `?token=` means this is the opening navigation: set the cookie
/// and redirect to a clean `/`. The token is then out of the URL bar, out of
/// history, and out of any `Referer` the page might later emit. `HttpOnly` keeps
/// it out of reach of script in our own page; `SameSite=Strict` keeps the browser
/// from attaching it to anyone else's request.
async fn index(State(app): State<Arc<App>>, Query(q): Query<IndexQuery>) -> Response {
    if let Some(token) = q.token {
        // Overwrites whatever this port's previous run left behind — the gate let
        // us here on the query token, so this is the credential that is current.
        let cookie = format!("{}={token}; Path=/; HttpOnly; SameSite=Strict", app.cookie);
        return ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response();
    }
    shell::index()
}

/// Any non-`/api` path: an embedded asset, or the SPA entry.
async fn static_asset(uri: axum::http::Uri) -> Response {
    shell::asset(uri.path())
}

async fn list_spaces(State(app): State<Arc<App>>) -> Response {
    Json(serde_json::json!({
        "spaces": spaces::list(&app.directory, &app.daemon).await
    }))
    .into_response()
}

#[derive(Deserialize)]
struct RpcQuery {
    /// The client has already asked [`crate::cli::destructive_question`] and been
    /// told yes. See [`rpc`].
    #[serde(default)]
    confirm: bool,
}

/// Browser adapters use disjoint routes: generic Space control enters
/// `POST /api/spaces/{id}/rpc`, while a package-owned protocol enters
/// `POST /api/spaces/{id}/worlds/{world}/rpc`.
///
/// One endpoint avoids a second REST projection. Product requests are decoded
/// by the product package and sent as opaque World calls; Space requests remain
/// on the host control protocol.
///
/// Selecting a space is what attaches its daemon, so this is also the first point
/// at which anything is started.
///
/// Three gates, in order:
///
/// 1. **`Subscribe` is refused.** It is a stream, not a one-shot: `control::request`
///    writes and reads exactly one line, so a subscribe here would decode a
///    `Doorbell` as a `Response` and fail confusingly. `GET /api/events` is the door.
/// 2. **An agent's space is observable, not operable.** Writes are refused with the
///    agent's name in the message. Reads through an agent's daemon are exactly the
///    observability they were scoped in for; a *write* would be signed by the agent
///    and land under its name. If you are a member of that space, write through
///    your own node and sign as yourself.
/// 3. **Destructive verbs keep the CLI's question.** `confirm_destructive` is a TTY
///    affordance: it refuses under `--json` because a pipe cannot be asked. A browser
///    can — it has a modal — so rather than bypass the gate or inherit the pipe's
///    refusal, the question comes back as a `409 confirm_required` and the UI asks
///    it. The string is `cli::destructive_question`'s, not a paraphrase, so the two
///    surfaces cannot disagree about what is dangerous.
///
/// Gate 3 protects against an *accident*, not an attacker: anything that can POST
/// `delete` can also POST `?confirm=1`. That is the same guarantee the CLI's prompt
/// gives, and it is worth being honest that it is the whole of it.
async fn world_rpc(
    State(app): State<Arc<App>>,
    Path((id, world)): Path<(String, String)>,
    Query(q): Query<RpcQuery>,
    Json(input): Json<serde_json::Value>,
) -> Response {
    let resolved = match app.directory.resolve(&id) {
        Ok(resolved) => resolved,
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                err_json(&error.to_string(), ErrorKind::NotFound),
            )
                .into_response();
        }
    };
    let registry = crate::world::client_packages();
    let package = registry.package_for_mount(&world).or_else(|| {
        replica::ids::WorldId::parse(&world).and_then(|world| registry.package_for_world(&world))
    });
    let Some(package) = package.cloned() else {
        return (
            StatusCode::NOT_FOUND,
            err_json(
                &format!("no client package is mounted for World '{world}'"),
                ErrorKind::NotFound,
            ),
        )
            .into_response();
    };
    let invocation = match package.parse_web(input) {
        Ok(invocation) => invocation,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                err_json(&error.to_string(), ErrorKind::Error),
            )
                .into_response();
        }
    };
    if let StationIdentity::Agent { name } = &resolved.identity {
        if invocation.access() != world_interface::ClientAccess::Query {
            return (
                StatusCode::FORBIDDEN,
                err_json(
                    &format!(
                        "{name}'s space is read-only here — a write would be signed as {name}. \
                         Open the same space through your own node to write as yourself."
                    ),
                    ErrorKind::Error,
                ),
            )
                .into_response();
        }
    }
    let scope = crate::daemon::ClientScope::pinned(resolved.address.orbit.clone());
    let host = crate::cli::PackageClientHost::new(&resolved.home, scope, None);
    if !q.confirm {
        // The same package-resolved question the CLI prompts with, so the modal
        // and the terminal cannot describe the same danger differently.
        match package.confirmation(&host, &invocation).await {
            Ok(Some(question)) => {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "kind": "confirm_required",
                        "question": question,
                    })),
                )
                    .into_response();
            }
            Ok(None) => {}
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    err_json(&error.to_string(), ErrorKind::Error),
                )
                    .into_response();
            }
        }
    }
    match package
        .execute(
            &host,
            invocation,
            world_interface::PresentationOptions {
                json: true,
                color: false,
            },
        )
        .await
    {
        Ok(output) => Json(output.value).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            err_json(&error.to_string(), ErrorKind::Error),
        )
            .into_response(),
    }
}

async fn rpc(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(q): Query<RpcQuery>,
    Json(input): Json<serde_json::Value>,
) -> Response {
    let resolved = match app.directory.resolve(&id) {
        Ok(resolved) => resolved,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                err_json(&e.to_string(), ErrorKind::NotFound),
            )
                .into_response()
        }
    };

    let req = match serde_json::from_value::<Request>(input) {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                err_json(&format!("bad request: {error}"), ErrorKind::Error),
            )
                .into_response();
        }
    };
    if matches!(req, Request::Subscribe { .. }) {
        return (
            StatusCode::BAD_REQUEST,
            err_json(
                "subscribe is a stream, not a request — use GET /api/events",
                ErrorKind::Error,
            ),
        )
            .into_response();
    }

    if let StationIdentity::Agent { name } = &resolved.identity {
        if !policy::is_read(&req) {
            return (
                StatusCode::FORBIDDEN,
                err_json(
                    &format!(
                        "{name}'s space is read-only here — a write would be signed as {name}. \
                         Open the same space through your own node to write as yourself."
                    ),
                    ErrorKind::Error,
                ),
            )
                .into_response();
        }
    }

    if !q.confirm {
        if let Some(question) = crate::cli::destructive_question(&req) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "kind": "confirm_required",
                    "question": question,
                })),
            )
                .into_response();
        }
    }

    let route = crate::control::station_route(resolved.address);
    if let Err(error) = crate::cli::ensure_lait_daemon().await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            err_json(&error.to_string(), ErrorKind::Error),
        )
            .into_response();
    }
    match app.daemon.request(route, &req, None).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            err_json(&e.to_string(), ErrorKind::Error),
        )
            .into_response(),
    }
}

/// The doorbell multiplex: one `EventSource` over every attached space.
///
/// Carries dirty *flags*, never state — the browser re-reads the authoritative
/// projection for each dirty scope, as required by the shared subscription contract. A
/// `Lagged` receiver is surfaced rather than hidden: the client's response is the
/// same rebaseline it already performs for reset or epoch changes, so
/// dropping frames under load is recoverable by construction.
async fn events(
    State(app): State<Arc<App>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    // The stream that ends when told to.
    //
    // A `BroadcastStream` will not: it yields `None` only when every sender has
    // dropped, and two outlive this handler — the one on `App` and the one the
    // pump holds. The item type is `Infallible`, so the body cannot fail out
    // either. Handed straight to `Sse`, an open SSE response is something
    // `with_graceful_shutdown` waits on forever.
    //
    // So the broadcast is pumped into a channel whose sender lives on a task
    // that selects on the stop signal. When that task returns, the sender drops
    // and the response body completes — termination by construction rather than
    // by an adapter nobody can see from here.
    let (tx, rx) = tokio::sync::mpsc::channel(EVENT_QUEUE);
    let mut doorbells = app.doorbells.subscribe();
    let mut stop = app.stop.subscribe();
    tokio::spawn(async move {
        if *stop.borrow_and_update() {
            return;
        }
        loop {
            let received = tokio::select! {
                biased;
                _ = stop.changed() => return,
                received = doorbells.recv() => received,
            };
            let event = match received {
                Ok(ViewerEvent::Doorbell(sd)) => Event::default()
                    .event("doorbell")
                    .json_data(sd)
                    .unwrap_or_else(|_| Event::default().event("lagged").data("encode")),
                Ok(ViewerEvent::Lagged) => Event::default().event("lagged").data("daemon"),
                // Surfaced rather than hidden: the client's response is the same
                // rebaseline it already performs for a reset or an epoch change,
                // so dropping frames under load is recoverable by construction.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    Event::default().event("lagged").data(n.to_string())
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            // A full queue means this client is not reading. Sending blocks
            // rather than dropping, because every frame is a dirty flag the
            // viewer needs; the client going away closes the channel and ends
            // the task, which is the only way out that is not a leak.
            if tx.send(Ok(event)).await.is_err() {
                return;
            }
        }
    });
    // Keep-alive so an idle space (no doorbells for minutes) doesn't look like a
    // dead connection to an intermediary or to the browser's own reconnect logic.
    Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

/// How many doorbells one browser may fall behind before its own reader becomes
/// the thing that blocks. Small: the frames are flags, and a client this far
/// behind is about to rebaseline anyway.
const EVENT_QUEUE: usize = 64;

/// Best-effort browser launch. Failure is not an error: the URL is already on
/// stdout, which is the contract; opening a window is a courtesy.
fn open_browser(url: &str) {
    let spawned = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "could not open a browser; use the printed URL");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    /// The real HTTP router, over an Orbit directory with no spaces.
    ///
    /// Every case below is refused (or served) by `gate` and the embedded-asset
    /// fallback, neither of which touches a daemon or the registry — so these run
    /// with no port, no store, and no process-wide env.
    fn app(token: &str) -> Router {
        let nowhere = std::path::PathBuf::from("/nonexistent-for-tests");
        router(Arc::new(App {
            guard: Guard::new(token.into(), 7717),
            directory: OrbitDirectory::new(nowhere.clone(), nowhere.clone(), true),
            daemon: LaitDaemonClient::at(nowhere),
            doorbells: tokio::sync::broadcast::channel(1).0,
            cookie: cookie_name(7717),
            stop: tokio::sync::watch::channel(false).0,
            content_permits: content::ContentStreamPermits::new(),
            bridge: bridge::BridgeHub::new(),
        }))
    }

    /// `GET /app.js` — the embedded bundle. Chosen because it proves the gate let
    /// the request *through* without needing a daemon behind it.
    fn req(headers: &[(&str, &str)], uri: &str) -> HttpRequest<Body> {
        let mut b = HttpRequest::builder().uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    }

    async fn status(token: &str, headers: &[(&str, &str)], uri: &str) -> StatusCode {
        app(token)
            .oneshot(req(headers, uri))
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_gate_refuses_and_admits_over_real_http() {
        const T: &str = "s3cret";

        // No credential at all.
        assert_eq!(
            status(T, &[("host", "127.0.0.1:7717")], "/app.js").await,
            StatusCode::UNAUTHORIZED,
        );

        // The rebinding signature: a **valid token** and the attacker's Host. This
        // is the case the whole ordering exists for — after a successful rebind the
        // browser believes they are us and hands over the cookie, so the token stops
        // being a secret they lack. Host is what they cannot launder.
        assert_eq!(
            status(
                T,
                &[("host", "evil.com"), ("authorization", "Bearer s3cret")],
                "/app.js",
            )
            .await,
            StatusCode::FORBIDDEN,
        );

        // Cross-origin caller that addresses us correctly.
        assert_eq!(
            status(
                T,
                &[
                    ("host", "127.0.0.1:7717"),
                    ("origin", "http://evil.com"),
                    ("authorization", "Bearer s3cret"),
                ],
                "/app.js",
            )
            .await,
            StatusCode::FORBIDDEN,
        );

        // …and the happy path actually serves.
        assert_eq!(
            status(
                T,
                &[
                    ("host", "127.0.0.1:7717"),
                    ("authorization", "Bearer s3cret")
                ],
                "/app.js",
            )
            .await,
            StatusCode::OK,
        );
    }

    /// The gate's *ordering*, which the case above cannot see.
    ///
    /// With a rebound Host **and** a valid token, both orderings answer 403 — so
    /// status alone proves nothing about which check ran. The distinguishing case is
    /// a rebound Host with **no** token: origin-first refuses the *host* (403) and
    /// never consults a credential; token-first refuses the *token* (401), which
    /// means it weighed the secret before establishing the request was even
    /// addressed to us. That is the invariant, and this is the only way to see it
    /// from outside.
    #[tokio::test]
    async fn the_origin_is_judged_before_the_credential() {
        assert_eq!(
            status("t", &[("host", "evil.com")], "/app.js").await,
            StatusCode::FORBIDDEN,
            "a rebound Host must be refused as a Host, not fall through to the token check",
        );
        // Same request, right host: now the credential is the thing that's wrong.
        assert_eq!(
            status("t", &[("host", "127.0.0.1:7717")], "/app.js").await,
            StatusCode::UNAUTHORIZED,
        );
    }

    /// The stale-cookie lockout, end to end.
    ///
    /// Cookies ignore the port, so a previous run leaves `lait_token_7717` in the
    /// jar for `127.0.0.1`. Cookie-first would 401 a freshly-printed link — and
    /// stay 401ing, since the page cannot clear an `HttpOnly` cookie it cannot read.
    #[tokio::test]
    async fn a_fresh_url_token_beats_a_stale_cookie_over_http() {
        const T: &str = "fresh";
        let res = app(T)
            .oneshot(req(
                &[
                    ("host", "127.0.0.1:7717"),
                    ("cookie", "lait_token_7717=stale"),
                ],
                "/?token=fresh",
            ))
            .await
            .unwrap();

        // Admitted, and handed the current credential to replace the stale one.
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let set = res
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(set.contains("lait_token_7717=fresh"), "got: {set}");
        assert!(
            set.contains("HttpOnly"),
            "the page must not be able to read it"
        );
        assert!(set.contains("SameSite=Strict"), "must not ride cross-site");

        // The stale cookie alone is still refused.
        assert_eq!(
            status(
                T,
                &[
                    ("host", "127.0.0.1:7717"),
                    ("cookie", "lait_token_7717=stale"),
                ],
                "/app.js",
            )
            .await,
            StatusCode::UNAUTHORIZED,
        );
    }

    /// An unknown path is the SPA's to route, not a 404.
    #[tokio::test]
    async fn unknown_paths_fall_back_to_the_app() {
        assert_eq!(
            status(
                "t",
                &[("host", "127.0.0.1:7717"), ("authorization", "Bearer t")],
                "/issues/SCRA-1",
            )
            .await,
            StatusCode::OK,
        );
    }

    #[test]
    fn query_param_finds_only_an_exact_key() {
        assert_eq!(query_param("token=abc", "token"), Some("abc".into()));
        assert_eq!(
            query_param("a=1&token=abc&b=2", "token"),
            Some("abc".into())
        );
        assert_eq!(query_param("a=1", "token"), None);
        // A key that merely ends with ours must not match.
        assert_eq!(query_param("xtoken=abc", "token"), None);
        assert_eq!(query_param("", "token"), None);
    }

    /// The precedence bug this exists to prevent, reproduced at the unit level.
    ///
    /// Cookies ignore the port, so a previous `lait serve` run leaves a stale
    /// `lait_token_*` in the jar for `127.0.0.1`. If the cookie were consulted
    /// before the query, clicking a freshly-printed URL would 401 — and stay
    /// 401ing, because the page cannot clear an HttpOnly cookie it cannot read.
    /// Found by restarting the server and opening the new link.
    #[test]
    fn a_fresh_url_token_beats_a_stale_cookie() {
        let guard = Guard::new("fresh".into(), 7717);
        let stale = auth::cookie_value("lait_token_7717=stale", "lait_token_7717");
        let query = query_param("token=fresh", "token");

        // `resolve_token` is what `gate` calls, so reordering the gate fails here.
        let presented = resolve_token(None, query.as_deref(), stale);
        assert_eq!(presented, Some("fresh"));
        assert!(guard.check_token(presented).is_ok());
    }

    #[test]
    fn bearer_outranks_everything_and_cookie_is_the_fallback() {
        assert_eq!(resolve_token(Some("b"), Some("q"), Some("c")), Some("b"));
        assert_eq!(resolve_token(None, Some("q"), Some("c")), Some("q"));
        assert_eq!(resolve_token(None, None, Some("c")), Some("c"));
        assert_eq!(resolve_token(None, None, None), None);
    }

    #[test]
    fn cookie_name_is_per_port_so_two_runs_do_not_share_a_jar_entry() {
        assert_ne!(cookie_name(7717), cookie_name(7801));
    }

    #[test]
    fn minted_tokens_are_64_hex_chars_and_not_repeated() {
        let a = mint_token();
        let b = mint_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "a per-run token must not be deterministic");
    }
}

#[cfg(test)]
mod gate_coverage {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    const TOKEN: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0";

    fn app() -> Router {
        let nowhere = std::path::PathBuf::from("/nonexistent-for-tests");
        router(Arc::new(App {
            guard: Guard::new(TOKEN.into(), 7717),
            directory: OrbitDirectory::new(nowhere.clone(), nowhere.clone(), true),
            daemon: LaitDaemonClient::at(nowhere),
            doorbells: tokio::sync::broadcast::channel(1).0,
            cookie: cookie_name(7717),
            stop: tokio::sync::watch::channel(false).0,
            content_permits: content::ContentStreamPermits::new(),
            bridge: bridge::BridgeHub::new(),
        }))
    }

    /// A concrete URI for a registered path, with the placeholders filled in.
    ///
    /// The gate runs before routing resolves parameters, so any value does — but
    /// it has to be *some* value, or axum answers 404 from outside the layer and
    /// the row proves nothing.
    fn concrete(path: &str) -> String {
        path.replace("{id}", "ws_x")
            .replace("{world}", "com.example.w")
    }

    fn request(method: Method, uri: &str, headers: &[(&str, &str)]) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder().uri(uri).method(match method {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Head => "HEAD",
        });
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Body::empty()).expect("request")
    }

    #[tokio::test]
    async fn every_registered_route_is_behind_the_gate() {
        // The point is not that these paths refuse — it is that the list the
        // router is built from is the list this walks. `Router::layer` wraps only
        // what precedes it, so a route registered after the layer ships with no
        // origin check and no token check and looks entirely normal. A test that
        // hand-copied the paths would stay green through exactly that mistake.
        assert!(!ROUTES.is_empty());
        for route in ROUTES {
            let (path, method) = (&route.path, &route.method);
            let uri = concrete(path);

            let unauthenticated = app()
                .oneshot(request(*method, &uri, &[("host", "127.0.0.1:7717")]))
                .await
                .expect("response");
            assert_eq!(
                unauthenticated.status(),
                StatusCode::UNAUTHORIZED,
                "{path} answered without a credential"
            );

            let rebound = app()
                .oneshot(request(
                    *method,
                    &uri,
                    &[
                        ("host", "evil.example.com"),
                        ("authorization", &format!("Bearer {TOKEN}")),
                    ],
                ))
                .await
                .expect("response");
            assert_eq!(
                rebound.status(),
                StatusCode::FORBIDDEN,
                "{path} served a rebound Host"
            );

            // And the credential does reach it — otherwise the two rows above
            // would pass just as well against a path that does not exist.
            let admitted = app()
                .oneshot(request(
                    *method,
                    &uri,
                    &[
                        ("host", "127.0.0.1:7717"),
                        ("authorization", &format!("Bearer {TOKEN}")),
                    ],
                ))
                .await
                .expect("response");
            assert_ne!(admitted.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert_ne!(admitted.status(), StatusCode::FORBIDDEN, "{path}");
        }
    }

    #[tokio::test]
    async fn a_content_url_will_not_take_its_credential_from_the_query_string() {
        // A download URL is a thing people paste, put in a `src`, and leave in
        // their history. A live credential in one is a credential in the URL
        // bar, in devtools, in the download list, and in the dev proxy's logs.
        //
        // `/` still accepts it, and must: the opening navigation carries the
        // token that way and `index` trades it for a cookie immediately.
        for route in ROUTES {
            let uri = format!("{}?token={TOKEN}", concrete(route.path));
            let response = app()
                .oneshot(request(route.method, &uri, &[("host", "127.0.0.1:7717")]))
                .await
                .expect("response");
            if route.query_token {
                assert_ne!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{} refused a credential it advertises accepting",
                    route.path
                );
            } else {
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{} took its credential out of the URL",
                    route.path
                );
            }
        }
    }

    #[test]
    fn a_path_pattern_matches_one_segment_and_not_a_path() {
        // The gate runs before routing resolves parameters, so it does its own
        // matching — and a `{id}` that swallowed a slash would let
        // `/api/spaces/a/b/content` be treated as a registered content route
        // (or not) by accident.
        assert!(path_matches("/api/spaces/{id}/rpc", "/api/spaces/ws_x/rpc"));
        assert!(!path_matches("/api/spaces/{id}/rpc", "/api/spaces/a/b/rpc"));
        assert!(!path_matches("/api/spaces/{id}/rpc", "/api/spaces//rpc"));
        assert!(!path_matches("/api/spaces/{id}/rpc", "/api/spaces/ws_x"));
        assert!(path_matches("/", "/"));
        assert!(!path_matches("/", "/anything"));
    }

    #[test]
    fn an_unregistered_path_does_not_accept_a_query_credential() {
        // The fallback serves the bundle to a browser that already has a
        // cookie. Defaulting the other way would make every future route opt
        // *out* of putting a live token in a URL, which is the wrong default to
        // have to remember.
        assert!(!accepts_query_token("/app.js"));
        assert!(!accepts_query_token("/api/spaces/ws_x/content/deadbeef"));
        assert!(accepts_query_token("/"));
    }

    #[tokio::test]
    async fn an_open_event_stream_ends_when_the_server_is_told_to_stop() {
        // The regression this exists for: `with_graceful_shutdown` waits on
        // in-flight responses, and an SSE body built straight off a broadcast
        // has no termination condition — its receiver ends only when every
        // sender drops, and two outlive the handler. A viewer left open was
        // therefore enough to make shutdown never return.
        let nowhere = std::path::PathBuf::from("/nonexistent-for-tests");
        let stop = tokio::sync::watch::channel(false).0;
        // Both senders are held here for the whole test, because a running
        // server holds them for its whole life. Letting the router drop them is
        // a second way for the stream to end, and it is the uninteresting one —
        // it would pass whether or not the stop signal works.
        let doorbells = tokio::sync::broadcast::channel(4).0;
        let app = Arc::new(App {
            guard: Guard::new(TOKEN.into(), 7717),
            directory: OrbitDirectory::new(nowhere.clone(), nowhere.clone(), true),
            daemon: LaitDaemonClient::at(nowhere),
            doorbells: doorbells.clone(),
            cookie: cookie_name(7717),
            stop: stop.clone(),
            content_permits: content::ContentStreamPermits::new(),
            bridge: bridge::BridgeHub::new(),
        });
        let response = router(app.clone())
            .oneshot(request(
                Method::Get,
                "/api/events",
                &[
                    ("host", "127.0.0.1:7717"),
                    ("authorization", &format!("Bearer {TOKEN}")),
                ],
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        // Draining the body is what a browser does. It must not finish yet.
        let body = response.into_body();
        let mut drained = tokio::spawn(async move { axum::body::to_bytes(body, 64 * 1024).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut drained)
                .await
                .is_err(),
            "the stream ended before anything asked it to"
        );

        let _ = stop.send(true);
        let ended = tokio::time::timeout(std::time::Duration::from_secs(5), drained)
            .await
            .expect("the event stream must end inside the shutdown deadline");
        ended.expect("join").expect("body");
        drop((app, doorbells));
    }

    #[tokio::test]
    async fn the_fallback_is_gated_too() {
        // Not in ROUTES, because it is not a route — but it is inside the layer,
        // and an unauthenticated bundle that 401s on every fetch afterwards is a
        // worse experience than an honest refusal.
        let response = app()
            .oneshot(request(
                Method::Get,
                "/app.js",
                &[("host", "127.0.0.1:7717")],
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
