//! The local app: bare `lait`'s HTTP surface, and the browser's Layer-B client.
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
//! registered Orbits. [`crate::daemon::Catalog`] supplies passive
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
mod content;
pub mod head;
pub mod orbits;
pub mod policy;
mod socket;

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
use crate::daemon::{Client, OrbitDoorbell};
use crate::orbits::{Catalog, ResolvedOrbit, StationIdentity};
use auth::{mint_token, Guard, Refusal};

/// The default port. Fixed rather than ephemeral so the URL is predictable and
/// the `Origin` allowlist has something stable to name; a collision is reported
/// rather than silently worked around, because a server that lands on a
/// *different* port than it was asked for is a footgun for anything that
/// bookmarked it.
pub const DEFAULT_PORT: u16 = 7717;

/// The cookie the browser trades its one-time URL token for.
///
/// Named per-port, because **cookies ignore the port**: `127.0.0.1:7717` and
/// `127.0.0.1:7801` are the same cookie origin, so a fixed name would have two
/// concurrent runs silently clobbering each other's credential —
/// whichever loaded last wins, and the other tab starts 401ing. The port is not a
/// security boundary here (the token is); it is what keeps two runs from being the
/// same jar entry.
fn cookie_name(port: u16) -> String {
    format!("lait_token_{port}")
}

struct App {
    registry: Arc<world_interface::WorldClientRegistry>,
    guard: Guard,
    /// The one World mount this head answers for.
    ///
    /// A request naming any other mount is refused rather than served. Without
    /// it a head answers for every mounted World, which makes "is this World
    /// running" a question about a shared process — and every control built on
    /// the answer, including stopping, a statement about the wrong thing.
    world: String,
    /// Where this head's web bundle is read from: the selected immutable World
    /// release, with no compiled product fallback.
    head: head::Source,
    directory: Catalog,
    daemon: Client,
    /// Which identity this server serves, carried rather than re-derived from
    /// the environment on every request that has to reach its daemon.
    selection: crate::config::Selection,
    doorbells: tokio::sync::broadcast::Sender<ViewerEvent>,
    cookie: String,
    /// The launch credentials this run has minted and not yet seen spent.
    ///
    /// On `App` because redemption must *consume*, and a store built per
    /// request could not: a ticket would answer as often as it was presented,
    /// which is the whole property that makes putting a credential in a URL
    /// defensible.
    launch_tickets: auth::LaunchTickets,
    /// Latched when this server begins shutting down.
    ///
    /// On `App` rather than passed down, because every long-lived response has
    /// to see it and they are constructed by handlers that share nothing else.
    stop: tokio::sync::watch::Sender<bool>,
    /// How many content transfers may be in flight at once.
    content_permits: content::ContentStreamPermits,
    /// The session socket's own fan-out. Deliberately not `doorbells`: one ring for what
    /// every tab must see, one for what only the tab that started a transfer
    /// cares about.
    socket: socket::Hub,
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
pub async fn run(
    port: u16,
    open: bool,
    json: bool,
    selection: crate::config::Selection,
    world: Option<String>,
) -> Result<()> {
    run_announced(port, open, selection, world, move |ready| {
        if json {
            // One line, then keep serving — the same shape `watch` has: a
            // long-running command whose first output is the fact you were
            // waiting for. Rust's stdout is a `LineWriter`, so the newline
            // flushes it to a piped parent without an explicit flush.
            println!(
                "{}",
                serde_json::json!({
                    "url": ready.url,
                    "token": ready.token,
                    "port": ready.port,
                    "world": ready.world,
                })
            );
        } else {
            println!("lait — your spaces at:\n  {}", ready.url);
            println!("(loopback only; this link carries a one-time token for this run)");
        }
    })
    .await
}

/// The readiness fact: the launch URL carrying the run token, the token
/// itself, and the bound port.
///
/// [`run`] prints it — stdout is the launcher's readiness contract, and
/// `viewer/scripts/dev.mjs` and `ci/smoke-p0.sh` both read that line. An
/// embedder that has no stdout to scrape — the iOS client is one process, and
/// a phone cannot read its own console — receives the same fact through
/// [`run_announced`]'s callback instead. Same moment, same guarantee: the
/// announcement lands **before** the listener starts accepting.
#[derive(Clone)]
pub struct Ready {
    pub url: String,
    pub token: String,
    pub port: u16,
    /// The one World this head serves.
    ///
    /// Announced rather than inferred, so a supervisor learns it from the head
    /// itself instead of from the arguments it hoped the head took. That is the
    /// difference between a stop that is a statement about a World and one that
    /// is a guess about a process.
    pub world: String,
}

/// [`run`], with the readiness line replaced by a callback.
///
/// This is the embedder's entry: everything `run` does, with the one
/// process-shaped assumption (stdout as the readiness channel) handed to the
/// caller instead. `run` is this function plus a `println!`.
pub async fn run_announced(
    port: u16,
    open: bool,
    selection: crate::config::Selection,
    world: Option<String>,
    announce: impl FnOnce(&Ready) + Send,
) -> Result<()> {
    run_until(port, open, selection, world, announce, shutdown_signal()).await
}

/// [`run_announced`], with the second process-shaped assumption handed over
/// too: *when to stop*.
///
/// [`run`] stops on a signal, because a process receives signals. An embedded
/// head does not — iOS delivers no SIGTERM, ever — and it has a need no
/// process has: the platform suspends the app and reclaims listener resources
/// while it sleeps, so the head must be able to step down before suspension
/// and come back after, as a transition rather than a crash. The caller hands
/// in the future that resolves when the head should leave; everything else —
/// the stop-before-drain ordering that lets the never-ending SSE and
/// WebSocket responses release — is identical to a signalled shutdown.
pub async fn run_until(
    port: u16,
    open: bool,
    selection: crate::config::Selection,
    world: Option<String>,
    announce: impl FnOnce(&Ready) + Send,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let identity = selection.identity_dir()?;
    let worlds = head::installations_root(&identity);
    let registry = Arc::new(crate::world::installed::load(&worlds)?.clients);
    run_until_with_registry(
        port,
        open,
        selection,
        world,
        registry,
        move |selected| head::activate(&worlds, selected.as_str()),
        announce,
        shutdown,
    )
    .await
}

async fn run_until_with_registry(
    port: u16,
    open: bool,
    selection: crate::config::Selection,
    world: Option<String>,
    registry: Arc<world_interface::WorldClientRegistry>,
    head_for: impl FnOnce(&replica::body::WorldId) -> head::Source,
    announce: impl FnOnce(&Ready) + Send,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    // Resolved before the listener binds. A build that cannot say which World
    // this head is refuses to be one, rather than coming up, announcing an
    // address, and answering for whatever mount a request happens to name.
    //
    // The same `pin` `lait mcp` uses, with one difference that is the whole
    // reason this is not a bare `pin` call: **its last rung is different, because
    // the two heads answer to different callers.**
    //
    // `pin(None)` refuses when a build hosts several Worlds, and for MCP that is
    // right — an editor binding names its World, and picking one for an agent
    // would put words in somebody's mouth. A browser head's caller is a person
    // typing `lait`, and refusing them because the build ships two Worlds is not
    // a safety property, it is the documented entry point declining to start.
    //
    // So this ladder ends one rung further down, at the selected install set's
    // primary. `--world` still selects any World, which is what gives each one
    // its own head; the default only decides which one bare `lait` opens.
    let identity = selection.identity_dir()?;
    let requested = world.as_deref().or_else(|| {
        (registry.packages().count() > 1)
            .then(|| {
                registry
                    .packages()
                    .next()
                    .map(world_interface::WorldClientPackage::mount)
            })
            .flatten()
    });
    let pinned = registry
        .pin(requested)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let pinned_mount = pinned.mount().to_owned();
    let pinned_world = pinned.world().clone();
    // Identity scoping, resolved once at startup from the invocation's own
    // selection rather than from a process-wide environment.
    let self_contained = selection.self_contained();
    let agents_base = crate::registry::agents_base(&crate::config::config_root()?);

    // Loopback only. Not `0.0.0.0`: that would hand the LAN an unauthenticated-
    // by-default view of every space on this machine, and the token is the only
    // thing that would stand between them and it.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .with_context(|| {
            format!("bind 127.0.0.1:{port} (is another `lait` already serving here?)")
        })?;
    let bound = listener.local_addr().context("read bound address")?;

    let token = mint_token()?;
    crate::host_client::ensure_lait_daemon(&selection)
        .await
        .context("start Lait daemon")?;
    let daemon = Client::for_selection(&selection)?;
    let (doorbells, _) = tokio::sync::broadcast::channel(256);
    // Created before anything that has to watch it. Every long-lived response
    // and every background task selects on this one channel.
    let (stop, _) = tokio::sync::watch::channel(false);
    // The head serves only the selected payload for its World. Resolved once
    // at start: a payload that
    // arrives later becomes live at the next head, which is the same
    // "applied at a boundary" rule the client tree follows.
    // This head's own World, not the build's first one. A Signage head serving
    // the Issues bundle would be the staging equivalent of the bug the pin
    // exists to close.
    let head = head_for(&pinned_world);
    let app = Arc::new(App {
        registry,
        head,
        world: pinned_mount.clone(),
        // The named form rides on the mount this head resolved above, so a World
        // is reachable by its own name and not only by an address. One head, one
        // World, one name.
        guard: Guard::for_world(token.clone(), bound.port(), &pinned_mount),
        directory: Catalog::new(identity, agents_base, self_contained),
        daemon: daemon.clone(),
        selection,
        doorbells: doorbells.clone(),
        cookie: cookie_name(bound.port()),
        launch_tickets: auth::LaunchTickets::new(),
        stop: stop.clone(),
        content_permits: content::ContentStreamPermits::new(),
        socket: socket::Hub::new(),
    });
    // The signal is fired before `shutdown_signal()` reaches axum, not after.
    // Graceful shutdown waits on in-flight responses, and this server has two
    // that never complete on their own: an SSE stream whose item type is
    // `Infallible` and whose broadcast receiver only ends when every sender
    // drops, and (from the session socket onward) a WebSocket. Handing axum a shutdown
    // future while those are open is a wait with no end, so the signal has to
    // reach *them* first.
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(pump_daemon_events(daemon, doorbells, stop.subscribe()));
    // One reader of the transient view for the whole server, not one per tab:
    // the hub fans each answer out, and a question nobody is holding is never
    // asked. See [`socket::pump_transient`] for why the socket has an inbound
    // direction at all.
    tasks.spawn(socket::pump_transient(app.clone(), stop.subscribe()));

    let url = format!("http://127.0.0.1:{}/?token={}", bound.port(), token);
    announce(&Ready {
        url: url.clone(),
        token: token.clone(),
        port: bound.port(),
        world: pinned_mount.clone(),
    });
    if open {
        // A launch ticket rather than the run token, and for the reason the
        // ticket exists: this URL is handed to a browser, so it lands in
        // history, in a synchronised profile, and in the shell's recent list.
        // The run token would still be current in every one of those places
        // tomorrow; a ticket is spent by the time the page has finished
        // loading.
        //
        // Falling back to the token URL if minting fails is deliberate — this
        // is a courtesy launch whose contract is "the URL is already on
        // stdout", and refusing to open a window because entropy was briefly
        // unavailable would be a worse trade than the one it protects against.
        let launch = app
            .launch_tickets
            .mint(None, auth::LAUNCH_TICKET_LIFETIME, now_ms())
            .map(|ticket| format!("http://127.0.0.1:{}/?ticket={}", bound.port(), ticket.secret))
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "could not mint a launch ticket; opening with the run token");
                url.clone()
            });
        open_browser(&launch);
    }

    let serve_result = axum::serve(listener, router(app))
        .with_graceful_shutdown({
            let stop = stop.clone();
            async move {
                shutdown.await;
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
    daemon: Client,
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
        backoff = backoff
            .checked_mul(2)
            .unwrap_or(PUMP_BACKOFF_CEILING)
            .min(PUMP_BACKOFF_CEILING);
    }
}

/// Stop the viewer adapter without stopping the independently owned Lait daemon.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    let _ = signal.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
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
    // Daemon-scoped, and it has to be: founding a Space, entering one from an
    // invite, and reading node-local settings all happen before there is a
    // space id to put in a path. Every other `/api` route is `/api/spaces/{id}`
    // and therefore unreachable at the only moment these matter.
    Route::open("/api/host/rpc", Method::Post),
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
    // Minting a launch credential requires the run credential, and never a
    // query one: a request that could ask for a ticket by URL would be a way to
    // turn one link into an endless supply of them.
    Route::no_query_token("/api/launch", Method::Post),
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

fn handler(route: &Route) -> Option<axum::routing::MethodRouter<Arc<App>>> {
    Some(match (route.path, route.method) {
        ("/", _) => get(index),
        ("/api/spaces", _) => get(list_spaces),
        ("/api/host/rpc", _) => post(host_rpc),
        ("/api/spaces/{id}/rpc", _) => post(rpc),
        ("/api/spaces/{id}/worlds/{world}/rpc", _) => post(world_rpc),
        ("/api/events", _) => get(events),
        ("/api/spaces/{id}/content", _) => post(content::upload),
        ("/api/spaces/{id}/content/{content}", Method::Head) => axum::routing::head(content::head),
        ("/api/spaces/{id}/content/{content}", _) => get(content::download),
        ("/api/session", _) => get(socket::session),
        ("/api/launch", _) => post(mint_launch),
        _ => return None,
    })
}

fn router(app: Arc<App>) -> Router {
    let mut router = Router::new();
    for route in ROUTES {
        if let Some(method) = handler(route) {
            router = router.route(route.path, method);
        }
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

    // A launch ticket is a credential in its own right, and a *better* one for
    // the opening navigation than the run token it replaces: 32 bytes of
    // entropy that stop being worth anything the first time they are presented.
    // It is admitted only where the run token's query form is admitted — the
    // opening navigation and nothing else — and `index` spends it there.
    //
    // Presence is enough to be let through; validity is decided by redemption,
    // which is the only place it *can* be decided, because deciding it here
    // would mean checking a single-use credential twice and spending it on the
    // check.
    let launching = accepts_query_token(req.uri().path())
        && req
            .uri()
            .query()
            .and_then(|q| query_param(q, "ticket"))
            .is_some();

    if !launching {
        if let Err(r) = app
            .guard
            .check_token(resolve_token(bearer, query.as_deref(), cookie))
        {
            return refuse(r);
        }
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
/// MCP client are reading one contract rather than two.
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
    /// A single-use launch credential minted by the client. See
    /// [`crate::serve::auth::LaunchTickets`].
    ticket: Option<String>,
}

/// The shell — and the one-time token handoff.
///
/// Arriving with `?token=` means this is the opening navigation: set the cookie
/// and redirect to a clean `/`. The token is then out of the URL bar, out of
/// history, and out of any `Referer` the page might later emit. `HttpOnly` keeps
/// it out of reach of script in our own page; `SameSite=Strict` keeps the browser
/// from attaching it to anyone else's request.
async fn index(
    State(app): State<Arc<App>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<IndexQuery>,
) -> Response {
    // A launch ticket is the client's handoff: single-use, Orbit-scoped and
    // short-lived, exchanged here for the ordinary session cookie. That
    // exchange is what "no persistent token exists" means in practice — what
    // travelled in the URL is spent by the time anybody reads it back out of
    // history.
    //
    // It also marks this browser as one the client sent, which is what the
    // overlay is gated on. A head somebody opened themselves has no client
    // context to draw and does not pretend to.
    if let Some(ticket) = q.ticket {
        let Some(redeemed) = app.launch_tickets.redeem(&ticket, now_ms()) else {
            // One answer for unknown, spent and expired. Telling them apart
            // tells a caller which guess was closer.
            return (
                StatusCode::UNAUTHORIZED,
                "this launch link has been used already, or has expired",
            )
                .into_response();
        };
        tracing::debug!(orbit = ?redeemed.orbit, "redeemed a launch ticket");
        let session = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict",
            app.cookie,
            app.guard.token()
        );
        let launched = format!(
            "{}=1; Path=/; HttpOnly; SameSite=Strict",
            client_cookie(&app.cookie)
        );
        // Appended, not inserted: two `Set-Cookie` headers with the same name
        // in an array would have the second replace the first, and the browser
        // would arrive holding the marker with no session to go with it.
        let mut cookies = axum::http::HeaderMap::new();
        for value in [session, launched] {
            match axum::http::HeaderValue::from_str(&value) {
                Ok(value) => {
                    cookies.append(header::SET_COOKIE, value);
                }
                Err(error) => {
                    tracing::error!(%error, "a launch cookie was not a header value");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "could not establish this session",
                    )
                        .into_response();
                }
            }
        }
        return (cookies, Redirect::to("/")).into_response();
    }
    if let Some(token) = q.token {
        // Overwrites whatever this port's previous run left behind — the gate let
        // us here on the query token, so this is the credential that is current.
        let cookie = format!("{}={token}; Path=/; HttpOnly; SameSite=Strict", app.cookie);
        return ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response();
    }
    shell::index(launched_by_client(&app, &headers), &app.head)
}

/// Any non-`/api` path: an asset from the selected World release, or its SPA entry.
async fn static_asset(
    State(app): State<Arc<App>>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    shell::asset(uri.path(), launched_by_client(&app, &headers), &app.head)
}

/// What a client asks for when it wants to open a World.
#[derive(serde::Deserialize)]
struct LaunchRequest {
    /// The Orbit the launch is scoped to, when the caller is opening one in
    /// particular. Optional: Astrolabe opens a World's head at its front page,
    /// where selecting a Space is the person's act — the redeemed ticket's
    /// orbit is informational (it is logged, never enforced; the path is what
    /// scopes a request), so a launch with no Orbit named is not a launch with
    /// something missing.
    #[serde(default)]
    orbit: Option<String>,
}

/// Mint one launch credential.
///
/// The tickets live here rather than in the client because redemption must
/// *consume*, and only the process that will be presented with a ticket can
/// spend it. A client minting its own would be issuing credentials against a
/// store nothing checks.
///
/// The caller already holds the run token — the gate saw to that — so this
/// grants nothing new. What it produces is *weaker* than what the caller has:
/// single-use, one Orbit, thirty seconds. That is the whole point, because this
/// is the one that travels in a URL.
async fn mint_launch(State(app): State<Arc<App>>, Json(request): Json<LaunchRequest>) -> Response {
    // An empty string and an absent field mean the same thing: no Orbit named.
    let orbit = request
        .orbit
        .as_deref()
        .map(str::trim)
        .filter(|orbit| !orbit.is_empty())
        .map(str::to_owned);
    match app
        .launch_tickets
        .mint(orbit, auth::LAUNCH_TICKET_LIFETIME, now_ms())
    {
        Ok(ticket) => Json(serde_json::json!({
            "ticket": ticket.secret,
            "orbit": ticket.orbit,
            "expiresAtMs": ticket.expires_at_ms,
        }))
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "could not mint a launch ticket");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not mint a launch credential",
            )
                .into_response()
        }
    }
}

/// The cookie that says "the client sent this browser here".
///
/// Derived from the session cookie's name so it is scoped to the same run and
/// the same port, and two heads on one machine cannot read each other's.
fn client_cookie(session: &str) -> String {
    format!("{session}_client")
}

/// Whether this request belongs to a browser the client launched.
///
/// The overlay is *client context*. A head a person opened themselves has none
/// to draw, and an overlay offering a route back to a client that is not there
/// is a control that cannot work — worse than absent, because it looks like a
/// feature.
fn launched_by_client(app: &App, headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| auth::cookie_value(value, &client_cookie(&app.cookie)))
        .is_some_and(|value| value == "1")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

async fn list_spaces(State(app): State<Arc<App>>) -> Response {
    Json(serde_json::json!({
        "spaces": orbits::list(&app.directory, &app.daemon).await
    }))
    .into_response()
}

#[derive(Deserialize)]
struct RpcQuery {
    /// The client has already asked [`crate::host_client::destructive_question`] and been
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
/// 3. **Destructive verbs have to be asked about.** The browser is the only surface
///    left that can ask, and it can — it has a modal — so the question comes back
///    as a `409 confirm_required` and the UI puts it to the user. The string is
///    `host_client::destructive_question`'s, not a paraphrase, so no two surfaces
///    can disagree about what is dangerous.
///
/// Gate 3 protects against an *accident*, not an attacker: anything that can POST
/// `delete` can also POST `?confirm=1`. It is worth being honest that this is the
/// whole of it.
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
    let registry = &app.registry;
    let package = registry.package_for_mount(&world).or_else(|| {
        replica::body::WorldId::parse(&world).and_then(|world| registry.package_for_world(&world))
    });
    // Mounted is not the same as *served here*. This head answers for one World
    // and refuses the rest by name, so a tab that wandered to another mount
    // learns it is at the wrong address instead of being quietly obliged — and
    // so stopping this head is a statement about one World.
    if package.is_some_and(|found| found.mount() != app.world) {
        return (
            StatusCode::NOT_FOUND,
            err_json(
                &format!(
                    "this head serves '{}' and not '{world}'; open that World's own head",
                    app.world
                ),
                ErrorKind::NotFound,
            ),
        )
            .into_response();
    }
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
    // `access()` is the class the package computed from the request it just
    // parsed, so asking costs no second decode on the call path.
    if invocation.access() != world_interface::ClientAccess::Query {
        if let Some(refusal) = borrowed_key_refusal(&app.directory, &resolved, "a write") {
            return refusal;
        }
    }
    let scope = crate::daemon::ClientScope::pinned(resolved.address.orbit.clone());
    // The address comes off the catalog the route already resolved: a World
    // call must not re-scan the store root to learn the Orbit it was addressed
    // to.
    let host = crate::host_client::PackageClientHost::new(
        &resolved.home,
        resolved.address.clone(),
        scope,
        None,
        app.selection.clone(),
    );
    if !q.confirm {
        // The package-resolved question, resolved once, so no two surfaces
        // can describe the same danger differently.
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
    match package.execute(&host, invocation).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            err_json(&error.to_string(), ErrorKind::Error),
        )
            .into_response(),
    }
}

/// Execute the latency-sensitive Issues editor calls through the exact same
/// package adapter as the HTTP endpoint, but return its JSON body to the
/// standing browser socket. Keeping this as a narrow allowlist means the
/// socket cannot accidentally become a second, prompt-less RPC surface.
async fn socket_editor_rpc(
    app: Arc<App>,
    space: String,
    input: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let command = input.get("cmd").and_then(serde_json::Value::as_str);
    if !matches!(
        command,
        Some("issue_text_splice" | "issue_text_checkpoint" | "issue_view")
    ) {
        return (
            StatusCode::FORBIDDEN,
            err_json(
                "the session socket accepts editor requests only",
                ErrorKind::Error,
            )
            .0,
        );
    }
    let response = world_rpc(
        State(app),
        Path((space, "issues".to_owned())),
        Query(RpcQuery { confirm: false }),
        Json(input),
    )
    .await;
    let status = response.status();
    let body = match axum::body::to_bytes(response.into_body(), socket::MAX_FRAME_BYTES).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            err_json("the editor request returned invalid JSON", ErrorKind::Error).0
        }),
        Err(_) => err_json("the editor response was too large", ErrorKind::Error).0,
    };
    (status, body)
}

/// The host plane: `POST /api/host/rpc`.
///
/// Every other `/api` route is `/api/spaces/{id}/…`, which is unanswerable at
/// the one moment this matters — founding a Space, or entering one from an
/// invite, is precisely the state in which there is no space id to name. So the
/// route is daemon-scoped, and the daemon is identity-scoped, which is the same
/// scope the token on this server already stands for.
///
/// The credential story is unchanged: this is registered in `ROUTES` like
/// everything else, so it is inside `gate` — same Origin check, same token,
/// same refusals. What is narrowed is the *vocabulary*: only host-plane
/// requests pass, because `ControlRoute::Daemon` also carries `Stop`, and a
/// page able to send that could shut down the server answering it.
async fn host_rpc(
    State(app): State<Arc<App>>,
    Query(q): Query<RpcQuery>,
    Json(input): Json<serde_json::Value>,
) -> Response {
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
    if !policy::is_host_plane(&req) {
        return (
            StatusCode::BAD_REQUEST,
            err_json(
                "this endpoint answers host-plane requests only — a Space request \
                 goes to POST /api/spaces/{id}/rpc",
                ErrorKind::Error,
            ),
        )
            .into_response();
    }
    if !q.confirm {
        if let Some(question) = crate::host_client::destructive_question(&req) {
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
    // No probe ahead of the call: the send is the probe. See
    // `host_client::request_daemon` — the healthy path is one round trip, and
    // a daemon that is not there is discovered by the failure it causes.
    match crate::host_client::host_request(&app.daemon, &app.selection, req).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => daemon_failure(&error),
    }
}

/// The refusal an act gets when this daemon would have to sign it with a key it
/// merely hosts, or `None` when the key would be the caller's own.
///
/// **Custody, not standing.** This is not a judgement about what kind of member
/// owns the Station, and it does not become redundant when a sponsored agent
/// holds write standing — it is not about standing at all. A Station whose
/// binding carries its own identity directory is signed with *that* seed
/// (`orbits::router` loads the seed from `resolved.identity_dir`), so a write
/// routed here on behalf of whoever holds this server's token would go out over
/// the agent's signature. Mechanics checks the *signer's* grants and the signer
/// would be the agent, so it approves; nothing behind this route asks the
/// question again. This is the only place it is asked, and the answer must be no
/// however wide anybody's grants become — the same rule
/// `orbits::bootstrap::admit` states for host requests.
///
/// It must never refuse a read: reading a hosted identity's board *authors*
/// nothing in the Space, and that is the whole reason it is browsable here.
/// Note the narrower claim — placement still loads that identity's seed to
/// stand its Station up (`orbits::router`), so reading brings the key onto the
/// wire even though it commits nothing. The fence is about authorship, not
/// about keeping the seed unread; a host that wanted the stronger property
/// would have to refuse placement, not refuse writes.
///
/// `Catalog::signs_with_own_seed` is the one spelling of the question, so the
/// enum shape stops being a proxy for it on four separate routes.
fn borrowed_key_refusal(
    directory: &Catalog,
    resolved: &ResolvedOrbit,
    act: &str,
) -> Option<Response> {
    if directory.signs_with_own_seed(resolved) {
        return None;
    }
    let holder = match &resolved.identity {
        StationIdentity::Agent { name } => name.clone(),
        StationIdentity::Own => resolved.home.display().to_string(),
    };
    Some(
        (
            StatusCode::FORBIDDEN,
            err_json(
                &format!(
                    "{holder}'s space is read-only here — {act} would be signed as {holder}. \
                     Open the same space through your own node to write as yourself."
                ),
                ErrorKind::Error,
            ),
        )
            .into_response(),
    )
}

/// A failure that never became a `Response`.
///
/// `503` when the daemon could not be reached at all (the caller may retry),
/// `400` otherwise. The split comes off the typed failure rather than the
/// message text, for the same reason exit codes do.
fn daemon_failure(error: &anyhow::Error) -> Response {
    let unreachable = error
        .downcast_ref::<crate::host_client::Failure>()
        .is_some_and(|failure| failure.code == 3)
        || error
            .downcast_ref::<crate::control::ForeignDaemon>()
            .is_some();
    let status = if unreachable {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, err_json(&format!("{error:#}"), ErrorKind::Error)).into_response()
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

    if !policy::is_read(&req) {
        if let Some(refusal) = borrowed_key_refusal(&app.directory, &resolved, "a write") {
            return refusal;
        }
    }

    if !q.confirm {
        if let Some(question) = crate::host_client::destructive_question(&req) {
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
    let envelope = crate::control::ClientRequest::routed(req, route, None);
    match crate::host_client::request_daemon(&app.daemon, &app.selection, &envelope).await {
        Ok(resp) => Json(resp).into_response(),
        Err(error) => daemon_failure(&error),
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

    fn released_head() -> head::Source {
        head::Source::activated(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("products/issues-app/assets/web"),
        )
    }

    /// The real HTTP router, over an Orbit directory with no spaces.
    ///
    /// Every case below is refused (or served) by `gate` and the World-asset
    /// fallback, neither of which touches a daemon or the registry — so these run
    /// with no port, no store, and no process-wide env.
    fn app(token: &str) -> Router {
        let nowhere = std::path::PathBuf::from("/nonexistent-for-tests");
        router(Arc::new(App {
            // Tests pin the build's own product World; the pin under test
            // is the refusal, not the choice.
            world: crate::world::ISSUES_MOUNT.to_owned(),
            registry: Arc::new(crate::world::client_packages().clone()),
            head: released_head(),
            guard: Guard::new(token.into(), 7717),
            directory: Catalog::new(nowhere.clone(), nowhere.clone(), true),
            daemon: Client::at(nowhere),
            selection: crate::config::Selection::default(),
            doorbells: tokio::sync::broadcast::channel(1).0,
            cookie: cookie_name(7717),
            launch_tickets: auth::LaunchTickets::new(),
            stop: tokio::sync::watch::channel(false).0,
            content_permits: content::ContentStreamPermits::new(),
            socket: socket::Hub::new(),
        }))
    }

    /// The same router, plus a handle on the run's launch tickets so a test can
    /// mint one and then watch it be spent.
    fn app_with_tickets(token: &str) -> (Router, Arc<App>) {
        let nowhere = std::path::PathBuf::from("/nonexistent-for-tests");
        let state = Arc::new(App {
            // Tests pin the build's own product World; the pin under test
            // is the refusal, not the choice.
            world: crate::world::ISSUES_MOUNT.to_owned(),
            registry: Arc::new(crate::world::client_packages().clone()),
            head: released_head(),
            guard: Guard::new(token.into(), 7717),
            directory: Catalog::new(nowhere.clone(), nowhere.clone(), true),
            daemon: Client::at(nowhere),
            selection: crate::config::Selection::default(),
            doorbells: tokio::sync::broadcast::channel(1).0,
            cookie: cookie_name(7717),
            launch_tickets: auth::LaunchTickets::new(),
            stop: tokio::sync::watch::channel(false).0,
            content_permits: content::ContentStreamPermits::new(),
            socket: socket::Hub::new(),
        });
        (router(state.clone()), state)
    }

    /// A launch link is spent on arrival and exchanged for the ordinary session
    /// cookie. That exchange is what "no persistent token exists" means: what
    /// travelled in the URL — and therefore into history, into a synchronised
    /// profile, and into the shell's recent list — is worth nothing by the time
    /// anybody reads it back out.
    #[tokio::test]
    async fn a_launch_link_is_exchanged_for_a_session_and_then_is_worthless() {
        let (router, state) = app_with_tickets("run-token");
        let ticket = state
            .launch_tickets
            .mint(
                Some("orb_one".into()),
                auth::LAUNCH_TICKET_LIFETIME,
                now_ms(),
            )
            .expect("mint");

        let first = router
            .clone()
            .oneshot(req(
                &[("host", "127.0.0.1:7717")],
                &format!("/?ticket={}", ticket.secret),
            ))
            .await
            .unwrap();
        assert!(
            first.status().is_redirection(),
            "a valid launch link did not land the browser on a clean URL: {}",
            first.status()
        );
        let cookies: Vec<String> = first
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect();
        assert!(
            cookies.iter().any(|cookie| cookie.contains("run-token")),
            "the launch did not hand over a session: {cookies:?}"
        );
        assert!(
            cookies.iter().any(|cookie| cookie.contains("_client=1")),
            "the launch did not mark this browser as one the client sent: {cookies:?}"
        );

        // Replay from history, which is exactly where a launch URL ends up.
        let again = router
            .oneshot(req(
                &[("host", "127.0.0.1:7717")],
                &format!("/?ticket={}", ticket.secret),
            ))
            .await
            .unwrap();
        assert_eq!(
            again.status(),
            StatusCode::UNAUTHORIZED,
            "a spent launch link was honoured a second time"
        );
    }

    /// `GET /app.js` — the selected World bundle. Chosen because it proves the gate let
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

    const HOSTED_TOKEN: &str = "hosted";

    /// A server whose catalog holds one Orbit that signs with a seed this daemon
    /// merely hosts, plus that Orbit's local id.
    ///
    /// The setup has to actually *resolve* as a hosted identity, or every fence
    /// test below would pass by never reaching a fence. The assertion on
    /// `borrowed_key_refusal` is that check, made once, before any route runs.
    fn hosted_identity_server() -> (Router, String) {
        let agents = std::path::PathBuf::from("/agents-for-tests");
        let store = agents.join("scout");
        let entry = crate::orbits::Entry {
            space: mechanics::ids::SpaceId::from_digest([7; 16]).to_string(),
            name: "Scout".into(),
            path: store.display().to_string(),
            origin: crate::orbits::Origin::Joined,
            host_nick: String::new(),
            last_opened: 0,
        };
        let directory = Catalog::with_entries(
            std::path::PathBuf::from("/identity-for-tests"),
            agents,
            false,
            vec![entry],
        );
        // Custody is what the routes read, not the enum shape beside it.
        let resolved = directory
            .resolve(crate::daemon::LocalOrbitId::for_store(&store).as_str())
            .expect("the agent's Orbit is visible to the human's directory");
        assert!(
            borrowed_key_refusal(&directory, &resolved, "a write").is_some(),
            "this Station signs with a seed the daemon merely hosts",
        );

        let orbit = resolved.address.orbit.as_str().to_string();
        let router = router(Arc::new(App {
            // Tests pin the build's own product World; the pin under test
            // is the refusal, not the choice.
            world: crate::world::ISSUES_MOUNT.to_owned(),
            registry: Arc::new(crate::world::client_packages().clone()),
            head: head::Source::unavailable(),
            guard: Guard::new(HOSTED_TOKEN.into(), 7717),
            directory,
            daemon: Client::at(std::path::PathBuf::from("/nonexistent-for-tests")),
            selection: crate::config::Selection::default(),
            doorbells: tokio::sync::broadcast::channel(1).0,
            cookie: cookie_name(7717),
            launch_tickets: auth::LaunchTickets::new(),
            stop: tokio::sync::watch::channel(false).0,
            content_permits: content::ContentStreamPermits::new(),
            socket: socket::Hub::new(),
        }));
        (router, orbit)
    }

    /// A credentialled POST, so each fence case below is refused by the fence and
    /// not by the gate in front of it.
    fn hosted_post(uri: String, body: Body) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method("POST")
            // No `confirm`: custody is settled before the destructive question is
            // even asked, which is the ordering that matters — a signature is not
            // something a prompt can license.
            .uri(uri)
            .header("host", "127.0.0.1:7717")
            .header("authorization", format!("Bearer {HOSTED_TOKEN}"))
            .header("content-type", "application/json")
            .body(body)
            .expect("request")
    }

    /// A hosted identity's Station is browsable and never writable through this
    /// server, over real HTTP, on **every** route that could sign.
    ///
    /// **The failure these pin:** the write would be routed to a Station whose
    /// binding carries its own seed, so it would go out over *that* identity's
    /// signature — and Mechanics, which checks the signer's standing, would
    /// approve it, because the signer holds write standing. These routes are the
    /// only places the custody question is asked, so deleting a check forges a
    /// signature rather than merely widening a permission. They answer before any
    /// daemon is contacted, which is also why these tests need none.
    #[tokio::test]
    async fn a_control_write_to_a_hosted_identitys_station_is_refused() {
        let (router, orbit) = hosted_identity_server();
        let body = serde_json::to_string(&Request::KeyRotate).expect("encode a write verb");
        let response = router
            .oneshot(hosted_post(
                format!("/api/spaces/{orbit}/rpc"),
                Body::from(body),
            ))
            .await
            .expect("route");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Declaring presence is a publication, so it is refused for an identity
    /// this daemon merely hosts.
    ///
    /// **The failure this pins:** `Watching` was classified as a read, and it is
    /// not one — carets, a typing flag, presence and the *uncommitted text* of a
    /// preview are published into the Space by the Station that signs for it. On
    /// a hosted Orbit that route let anything holding this server's token put
    /// attacker-chosen text into somebody else's Space under their name. The
    /// browser's own presence never came this way: `GET /api/session` declares
    /// it, and asks custody first.
    #[tokio::test]
    async fn declaring_presence_on_a_hosted_identitys_station_is_refused() {
        let (router, orbit) = hosted_identity_server();
        let body = serde_json::to_string(&Request::Watching {
            world: "com.lait.issues".into(),
            bodies: vec![[1; 16]],
            carets: vec![],
            typing: vec![],
            previews: vec![],
        })
        .expect("encode a declaration");
        let response = router
            .oneshot(hosted_post(
                format!("/api/spaces/{orbit}/rpc"),
                Body::from(body),
            ))
            .await
            .expect("route");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_world_write_to_a_hosted_identitys_station_is_refused() {
        let (router, orbit) = hosted_identity_server();
        // A command-class Issues call: the package classifies it, and the route
        // asks custody the moment the class is not `Query`. A read on the same
        // route must still be served, which is the point of the pair below.
        let response = router
            .clone()
            .oneshot(hosted_post(
                format!("/api/spaces/{orbit}/worlds/issues/rpc"),
                Body::from(r#"{"cmd":"issue_start","reff":"ENG-1"}"#),
            ))
            .await
            .expect("route");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // The read is not refused by custody — it gets as far as the daemon that
        // is not there, which is a different failure and the one we want.
        let read = router
            .oneshot(hosted_post(
                format!("/api/spaces/{orbit}/worlds/issues/rpc"),
                Body::from(r#"{"cmd":"board"}"#),
            ))
            .await
            .expect("route");
        assert_ne!(
            read.status(),
            StatusCode::FORBIDDEN,
            "looking at a hosted identity's board signs nothing",
        );
    }

    #[tokio::test]
    async fn a_content_upload_to_a_hosted_identitys_station_is_refused() {
        let (router, orbit) = hosted_identity_server();
        let response = router
            .oneshot(hosted_post(
                format!("/api/spaces/{orbit}/content?len=4"),
                Body::from("data"),
            ))
            .await
            .expect("route");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
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
    /// Cookies ignore the port, so a previous run leaves a stale
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
        let a = mint_token().expect("mint first token");
        let b = mint_token().expect("mint second token");
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
            // Tests pin the build's own product World; the pin under test
            // is the refusal, not the choice.
            world: crate::world::ISSUES_MOUNT.to_owned(),
            registry: Arc::new(crate::world::client_packages().clone()),
            head: head::Source::unavailable(),
            guard: Guard::new(TOKEN.into(), 7717),
            directory: Catalog::new(nowhere.clone(), nowhere.clone(), true),
            daemon: Client::at(nowhere),
            selection: crate::config::Selection::default(),
            doorbells: tokio::sync::broadcast::channel(1).0,
            cookie: cookie_name(7717),
            launch_tickets: auth::LaunchTickets::new(),
            stop: tokio::sync::watch::channel(false).0,
            content_permits: content::ContentStreamPermits::new(),
            socket: socket::Hub::new(),
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
            // Tests pin the build's own product World; the pin under test
            // is the refusal, not the choice.
            world: crate::world::ISSUES_MOUNT.to_owned(),
            registry: Arc::new(crate::world::client_packages().clone()),
            head: head::Source::unavailable(),
            guard: Guard::new(TOKEN.into(), 7717),
            directory: Catalog::new(nowhere.clone(), nowhere.clone(), true),
            daemon: Client::at(nowhere),
            selection: crate::config::Selection::default(),
            doorbells: doorbells.clone(),
            cookie: cookie_name(7717),
            launch_tickets: auth::LaunchTickets::new(),
            stop: stop.clone(),
            content_permits: content::ContentStreamPermits::new(),
            socket: socket::Hub::new(),
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
