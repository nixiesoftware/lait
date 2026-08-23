#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "Compatibility process framing validates byte boundaries and lengths before its bounded codec operations; the encoding must remain byte-identical."
)]

//! The identity-scoped Lait daemon and its single local control endpoint.
//!
//! The host owns no Space state directly. It owns one [`crate::orbits::Router`], which
//! lazily places Stations into addressed Orbits and keeps their StationHosts
//! inside this process. Historical per-home control sockets remain behind those
//! process hosts as compatibility adapters, but every request a head sends
//! enters through this endpoint first.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    ListenerOptions,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::{acquire_daemon_lock, DaemonLock};
use crate::control::{
    self, ClientRequest, ControlRoute, Endpoint, Request, Response, WorldClientRequest,
    CONTROL_PROTOCOL_VERSION,
};
use crate::orbital::WorldPackages;
#[cfg(test)]
use comms::TransportFactory;
use runtime::world::call::{Call, Code, Reply};

use super::OrbitDoorbell;
use crate::orbits::Catalog;
use crate::orbits::{ContentPlacement, Router};

/// A client for the current identity's one Lait daemon.
#[derive(Debug, Clone)]
pub struct Client {
    home: PathBuf,
}

impl Client {
    /// The daemon for an explicitly selected identity.
    ///
    /// There is deliberately no ambient constructor. A head serving several
    /// identities out of one process cannot address more than one daemon if the
    /// home comes from process-global environment, so the home is an argument
    /// on every path that reaches a daemon.
    pub fn for_selection(selection: &crate::config::Selection) -> Result<Self> {
        Ok(Self {
            home: selection.daemon_home()?,
        })
    }

    pub fn at(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub async fn probe(&self) -> control::Probe {
        control::probe(&self.home).await
    }

    pub async fn request(
        &self,
        route: ControlRoute,
        request: &Request,
        act_as: Option<&str>,
    ) -> Result<Response> {
        control::request_as_routed(&self.home, request, Some(route), act_as).await
    }

    /// Send an envelope the caller already owns, so a caller that may have to
    /// re-send does not clone the request to keep a copy.
    pub async fn send(&self, envelope: &ClientRequest) -> Result<Response> {
        control::send(&self.home, envelope).await
    }

    /// Send a product-neutral call without importing that product's request or
    /// response schema into the daemon protocol.
    pub async fn call_world(
        &self,
        route: ControlRoute,
        call: Call,
        act_as: Option<&str>,
    ) -> Result<Reply> {
        control::call_world(&self.home, route, call, act_as).await
    }

    /// The World counterpart of [`Client::send`]. The World-call path is the
    /// latency-critical one; it must not copy a call payload for a retry.
    pub async fn call_world_envelope(&self, envelope: &WorldClientRequest) -> Result<Reply> {
        control::call_world_envelope(&self.home, envelope).await
    }

    /// Query an already-running Station without placing a vacant Orbit.
    pub async fn request_if_running(
        &self,
        route: ControlRoute,
        request: &Request,
    ) -> Result<Response> {
        control::request_routed_if_running(&self.home, request, route).await
    }

    pub async fn subscribe_space(
        &self,
        route: ControlRoute,
        since: u64,
    ) -> Result<control::Subscription> {
        control::subscribe_routed(&self.home, since, Some(route)).await
    }

    pub async fn subscribe_catalog(&self) -> Result<OrbitSubscription> {
        let name = control::control_name(&self.home)?;
        let mut stream = LocalStream::connect(name)
            .await
            .context("connect to Lait daemon")?;
        let envelope =
            ClientRequest::routed(Request::Subscribe { since: 0 }, ControlRoute::Daemon, None);
        let mut line = serde_json::to_string(&envelope).context("encode catalog subscribe")?;
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .await
            .context("write catalog subscribe")?;
        stream.flush().await.ok();
        Ok(OrbitSubscription {
            reader: BufReader::new(stream),
        })
    }

    pub async fn subscribe_live(
        &self,
        route: ControlRoute,
        world: String,
        body: Option<[u8; 16]>,
    ) -> Result<control::LiveSubscription> {
        control::subscribe_live_routed(&self.home, route, world, body).await
    }
}

/// A catalog-wide stream of Orbit-tagged invalidations.
pub struct OrbitSubscription {
    reader: BufReader<LocalStream>,
}

impl OrbitSubscription {
    pub async fn next(&mut self) -> Result<Option<OrbitDoorbell>> {
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .await
            .context("read Orbit doorbell")?;
        if read == 0 {
            return Ok(None);
        }
        serde_json::from_str(line.trim())
            .context("decode Orbit doorbell")
            .map(Some)
    }
}

/// What a served request leaves behind: a connection ready for the next one, or
/// nothing.
///
/// The halves ride inside `Next` because an operation that takes one of them —
/// a content upload owns the read half, a subscription owns the write half —
/// cannot hand it back, and the type is what makes that impossible to forget.
enum Flow {
    Next(
        BufReader<tokio::io::ReadHalf<LocalStream>>,
        tokio::io::WriteHalf<LocalStream>,
    ),
    Close,
}

/// The identity-scoped local listener and control-protocol service.
///
/// It owns framing, connection tasks, streaming, and delegation. Placement
/// state remains owned by [`Router`].
pub(crate) struct Listener {
    router: Arc<Router>,
    display: Arc<crate::display::DisplayRuntime>,
    stopping: tokio::sync::watch::Sender<bool>,
}

impl Listener {
    pub(crate) fn new(router: Arc<Router>, display: Arc<crate::display::DisplayRuntime>) -> Self {
        Self {
            router,
            display,
            stopping: tokio::sync::watch::channel(false).0,
        }
    }

    pub(crate) fn begin_stop(&self) {
        self.stopping.send_replace(true);
    }

    pub(crate) fn subscribe_stop(&self) -> tokio::sync::watch::Receiver<bool> {
        self.stopping.subscribe()
    }

    pub(crate) async fn serve(self: Arc<Self>, home: &Path) -> Result<()> {
        let control = control::control_name(home)?;
        #[cfg(unix)]
        let _ = std::fs::remove_file(crate::config::socket_path(home));
        let listener = ListenerOptions::new()
            .name(control)
            .create_tokio()
            .context("bind Lait daemon control channel")?;
        tracing::info!("Lait daemon online");

        let mut stop = self.stopping.subscribe();
        let mut connections = tokio::task::JoinSet::new();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                },
                accepted = listener.accept() => match accepted {
                    Ok(stream) => {
                        let endpoint = self.clone();
                        connections.spawn(async move { endpoint.handle_conn(stream).await });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Lait daemon accept failed");
                        break;
                    }
                },
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::warn!(%error, "Lait daemon connection task failed");
                    }
                }
            }
        }

        self.begin_stop();
        Phase::DrainingConnections.enter();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(result) = connections.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(%error, "Lait daemon connection failed during shutdown");
                }
            }
        })
        .await
        .map_err(|_| {
            anyhow!(
                "Lait daemon connections did not drain during shutdown ({} remain)",
                connections.len()
            )
        })?;
        Ok(())
    }

    /// Serve one content call, in whichever process owns the Station.
    ///
    /// An owned placement is served here: the body crosses from the socket to
    /// the sealer without leaving this address space. An attached one is
    /// proxied byte for byte down the per-Orbit socket — never refused, because
    /// `Attached` is a reachable placement and a surface that works only when
    /// the Station happens to be in-process is a surface with a hidden
    /// precondition.
    async fn serve_content(
        self: Arc<Self>,
        reader: BufReader<tokio::io::ReadHalf<LocalStream>>,
        mut write_half: tokio::io::WriteHalf<LocalStream>,
        request: control::ContentClientRequest,
    ) {
        let placement = match self.router.content_placement(&request.route).await {
            Ok(placement) => placement,
            Err(error) => {
                let _ = write_line(
                    &mut write_half,
                    &control::ContentReply::error(
                        control::ContentErrorCode::Invalid,
                        format!("{error:#}"),
                    ),
                )
                .await;
                return;
            }
        };

        match placement {
            ContentPlacement::InProcess { host, address } => {
                let ceiling = host.max_content_len();
                if request.body_len > ceiling {
                    let _ = write_line(
                        &mut write_half,
                        &control::ContentReply::error(
                            control::ContentErrorCode::Bounds,
                            format!(
                                "this Station accepts at most {ceiling} bytes in one \
                                 content; the request declared {}",
                                request.body_len
                            ),
                        ),
                    )
                    .await;
                    return;
                }
                let expects_body = matches!(request.content, control::ContentCall::Write { .. });
                let (body, pump) = control::upload_body(reader, request.body_len);
                let call = request.content.clone();
                let mut stopping = self.stopping.subscribe();
                let work = tokio::task::spawn_blocking(move || {
                    host.content_call(&address, &call, expects_body.then_some(body))
                });
                let (_, sealed) = tokio::join!(pump, work);
                let (reply, payload) = sealed.unwrap_or_else(|_| {
                    (
                        control::ContentReply::error(
                            control::ContentErrorCode::Storage,
                            "the content call did not finish",
                        ),
                        Vec::new(),
                    )
                });
                // A stop that landed mid-transfer is reported rather than
                // answered. The drain that follows is bounded and hard-fails if
                // a connection outlives it, so a caller reading "written" from
                // a daemon that is going away is the worse of the two answers.
                if *stopping.borrow_and_update()
                    && !matches!(reply, control::ContentReply::ContentError { .. })
                {
                    let _ = write_line(
                        &mut write_half,
                        &control::ContentReply::error(
                            control::ContentErrorCode::Storage,
                            "this daemon is shutting down",
                        ),
                    )
                    .await;
                    return;
                }
                if write_line(&mut write_half, &reply).await.is_err() {
                    return;
                }
                if !payload.is_empty() {
                    let _ = write_half.write_all(&payload).await;
                    let _ = write_half.flush().await;
                }
            }
            ContentPlacement::Attached { home } => {
                match proxy_content(&home, reader, &mut write_half, &request).await {
                    Ok(()) => {}
                    // Only before the answer's header has been forwarded. Once
                    // the client has been told how many bytes follow, it is
                    // inside `read_exact` for exactly that many — so a JSON
                    // error appended here is not an error message, it is the
                    // first bytes of the file, and the client cannot tell.
                    // After the header, the only honest report is the truncated
                    // stream itself.
                    Err(ProxyFailure::BeforeHeader(error)) => {
                        let _ = write_line(
                            &mut write_half,
                            &control::ContentReply::error(
                                control::ContentErrorCode::Storage,
                                error,
                            ),
                        )
                        .await;
                    }
                    Err(ProxyFailure::AfterHeader) => {}
                }
            }
        }
    }

    /// Serve a connection until the client stops sending or an operation takes
    /// the stream over.
    ///
    /// One connection carries many requests. It used to carry exactly one, so a
    /// head answering a browser paid a fresh connect for the board, another for
    /// status, another for members — every time. Clients now park a connection
    /// between requests, which only works if this side keeps reading; the
    /// framing already allowed it, because a request is one bounded line and a
    /// response is one line.
    async fn handle_conn(self: Arc<Self>, stream: LocalStream) {
        let (read_half, write_half) = tokio::io::split(stream);
        let (mut reader, mut writer) = (BufReader::new(read_half), write_half);
        loop {
            match self.clone().serve_one(reader, writer).await {
                Flow::Next(next_reader, next_writer) => {
                    (reader, writer) = (next_reader, next_writer)
                }
                Flow::Close => return,
            }
        }
    }

    /// One request, and what the connection does afterwards.
    ///
    /// The halves travel in and out by value because some operations *take*
    /// them: a content upload owns the read half for the length of the body, a
    /// subscription owns the write half until the client goes away. Those
    /// answer [`Flow::Close`] and the loop above ends, which is the same thing
    /// that used to happen by returning.
    async fn serve_one(
        self: Arc<Self>,
        mut reader: BufReader<tokio::io::ReadHalf<LocalStream>>,
        mut write_half: tokio::io::WriteHalf<LocalStream>,
    ) -> Flow {
        use tokio::io::AsyncReadExt;

        let mut line = String::new();
        {
            // Bounded, because a request header is a bounded thing. An
            // unbounded `read_line` grows until it finds a newline or the
            // sender stops, so a client that opens the socket and sends no
            // newline is a memory attack that needs no authorization.
            //
            // Timed, because a parked connection and an abandoned one look
            // identical until one of them speaks. A client's reuse window is a
            // quarter of this timeout, so a connection reaped here is one no
            // client still intends to use.
            //
            // Woken by the stop signal, because shutdown joins these tasks. A
            // connection idling out its window is doing nothing, but it is
            // still a task to join, and waiting for it would make every
            // shutdown take the whole window.
            let mut stopping = self.stopping.subscribe();
            if *stopping.borrow_and_update() {
                return Flow::Close;
            }
            let mut bounded = (&mut reader).take(control::MAX_CONTROL_LINE_BYTES);
            let read = async {
                tokio::select! {
                    read = bounded.read_line(&mut line) => read,
                    _ = stopping.changed() => Ok(0),
                }
            };
            match tokio::time::timeout(control::IDLE_CONNECTION_TIMEOUT, read).await {
                // EOF, a read error, a client that stopped speaking, or this
                // daemon going away: there is nothing to answer and nothing to
                // keep open for.
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return Flow::Close,
                Ok(Ok(_)) => {}
            }
        }
        let value = match serde_json::from_str::<serde_json::Value>(line.trim()) {
            Ok(value) => value,
            Err(error) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err(format!("bad request: {error}")),
                )
                .await;
                // Malformed input ends the connection rather than continuing on
                // it: a sender that cannot frame a request is not one whose
                // next bytes should be trusted to start where this one stopped.
                return Flow::Close;
            }
        };

        if value.get("content").is_some() {
            let request: control::ContentClientRequest = match serde_json::from_value(value) {
                Ok(request) => request,
                Err(error) => {
                    let _ = write_line(
                        &mut write_half,
                        &control::ContentReply::error(
                            control::ContentErrorCode::Invalid,
                            format!("bad content call: {error}"),
                        ),
                    )
                    .await;
                    return Flow::Close;
                }
            };
            // Takes the read half for the length of the body.
            self.serve_content(reader, write_half, request).await;
            return Flow::Close;
        }

        if value.get("call").is_some() {
            let control::WorldCallFrame {
                route,
                act_as,
                call: header,
            } = match serde_json::from_value(value) {
                Ok(request) => request,
                Err(error) => {
                    let _ = write_line(
                        &mut write_half,
                        &Response::err(format!("bad World call: {error}")),
                    )
                    .await;
                    return Flow::Close;
                }
            };
            // The payload rides behind the header. Every failure from here to
            // the end of the read closes the connection rather than answering
            // on it: the declared bytes are either consumed exactly or the
            // stream's position is unknown, and there is no third state a
            // reused connection could survive.
            let want = match control::refuse_oversized_payload(header.len) {
                Ok(want) => want,
                Err(error) => {
                    let _ = write_line(&mut write_half, &Response::err(format!("{error:#}"))).await;
                    return Flow::Close;
                }
            };
            let mut payload = vec![0u8; want];
            if reader.read_exact(&mut payload).await.is_err() {
                return Flow::Close;
            }
            let call = match Call::new(header.world, header.operation, header.version, payload) {
                Ok(call) => call,
                Err(error) => {
                    let _ = write_line(
                        &mut write_half,
                        &Response::err(format!("bad World call: {error}")),
                    )
                    .await;
                    return Flow::Close;
                }
            };
            let reply = self
                .router
                .call_world(route, &call, act_as.as_deref())
                .await
                .unwrap_or_else(|error| {
                    Reply::error(&call, Code::InvalidCall, format!("{error:#}"))
                });
            let (frame, payload) = control::frame_reply(reply);
            if write_line(&mut write_half, &frame).await.is_err() {
                return Flow::Close;
            }
            if write_half.write_all(&payload).await.is_err() {
                return Flow::Close;
            }
            let _ = write_half.flush().await;
            return Flow::Next(reader, write_half);
        }

        let ClientRequest {
            route,
            if_running,
            act_as,
            request,
        } = match serde_json::from_value::<ClientRequest>(value) {
            Ok(request) => request,
            Err(error) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err(format!("bad request: {error}")),
                )
                .await;
                return Flow::Close;
            }
        };

        if matches!(request, Request::Hello { .. }) {
            let _ = write_line(
                &mut write_half,
                &Response::Hello {
                    protocol_version: CONTROL_PROTOCOL_VERSION,
                    // Answered here, on the daemon's own connection path, so it
                    // describes the process actually holding this home.
                    build: Some(crate::control::BuildFingerprint::here()),
                },
            )
            .await;
            return Flow::Next(reader, write_half);
        }

        let Some(route) = route else {
            let _ = write_line(
                &mut write_half,
                &Response::err("the Lait daemon requires an explicit control route"),
            )
            .await;
            return Flow::Close;
        };

        if if_running {
            let space = match &route {
                ControlRoute::Orbit { address } => Some(address.space.clone()),
                _ => None,
            };
            let mut response = match (&route, &request) {
                (
                    ControlRoute::Orbit { .. },
                    Request::Status
                    | Request::Id
                    | Request::ConfigReload
                    | Request::Who
                    // Live sits here for the same reason Who does: it reads
                    // the Station's own transient table — who is doing what
                    // right now — and neither journals nor replays anything.
                    // A glance at who has a World open must never be the act
                    // that places one.
                    | Request::Live { .. }
                    | Request::Storage
                    | Request::WorldsActive
                    | Request::Diagnose { .. },
                ) => self
                    .router
                    .request_running(route, &request)
                    .await
                    .unwrap_or_else(|error| Response::err(format!("{error:#}"))),
                _ => Response::err(
                    "passive dispatch is only available for supported observation requests \
                     through an explicit Space route",
                ),
            };
            // Presence sampled passively still carries names — the book
            // decorates this path exactly as it does the placed one, or the
            // client would read "no name" as a fact about the peer when it
            // was a fact about the route.
            if let Some(space) = &space {
                if matches!(
                    response,
                    Response::Who { .. } | Response::Members { .. } | Response::Seeds { .. }
                ) {
                    if let Ok(book) = self.router.book() {
                        book.decorate(space, &mut response);
                    }
                }
            }
            let _ = write_line(&mut write_half, &response).await;
            return Flow::Next(reader, write_half);
        }

        match (route, request) {
            (ControlRoute::Daemon, Request::Stop) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::Ok {
                        message: Some("stopping Lait daemon".into()),
                    },
                )
                .await;
                self.begin_stop();
                Flow::Close
            }
            // Answer first, then go. The head that sent this stands a fresh
            // daemon up on its next send, so the reply has to be on the wire
            // before the socket closes or the restart looks like a crash.
            (ControlRoute::Daemon, Request::HostRestart) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::Host(crate::control::HostReply::Restarting {
                        pid: Some(std::process::id()),
                    }),
                )
                .await;
                self.begin_stop();
                Flow::Close
            }
            (ControlRoute::Daemon, Request::Subscribe { .. }) => {
                self.stream_catalog(write_half).await;
                Flow::Close
            }
            // The host plane: formation, node-local state, orientation. It is
            // served here rather than behind a Station because most of it runs
            // before a Station could exist — and because running it in this
            // process is what stops it racing this process for the store lock.
            (ControlRoute::Daemon, request)
                if crate::daemon::correspondence::is_correspondence_request(&request) =>
            {
                let response = self.router.correspondence().handle(request).await;
                let _ = write_line(&mut write_half, &response).await;
                Flow::Next(reader, write_half)
            }
            (ControlRoute::Daemon, request)
                if crate::daemon::address_book::is_book_request(&request) =>
            {
                let response = match self.router.book() {
                    Ok(book) => book.handle(request, &self.router).await,
                    Err(error) => Response::err(error),
                };
                let _ = write_line(&mut write_half, &response).await;
                Flow::Next(reader, write_half)
            }
            (ControlRoute::Daemon, request) => {
                let response = match self.display.handle_control(&request).await {
                    Some(response) => response,
                    None => crate::orbits::bootstrap::dispatch(&self.router, request)
                        .await
                        .unwrap_or_else(|| Response::err("request has no daemon-scoped handler")),
                };
                let _ = write_line(&mut write_half, &response).await;
                Flow::Next(reader, write_half)
            }
            (route @ ControlRoute::Orbit { .. }, Request::Subscribe { since }) => {
                self.stream_space(write_half, route, since).await;
                Flow::Close
            }
            (route @ ControlRoute::Orbit { .. }, Request::LiveSubscribe { world, body }) => {
                self.stream_live(write_half, route, world, body).await;
                Flow::Close
            }
            (ControlRoute::World { .. }, Request::Subscribe { .. }) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err("subscriptions require a Space route"),
                )
                .await;
                Flow::Next(reader, write_half)
            }
            (ControlRoute::World { .. }, Request::LiveSubscribe { .. }) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err("Live subscriptions require a Space route"),
                )
                .await;
                Flow::Next(reader, write_half)
            }
            (
                ControlRoute::Orbit { address } | ControlRoute::World { address, .. },
                Request::SponsorWatch { heads },
            ) => {
                // Identity-scoped, like the ask itself. The Station has no
                // file for this; Exec Watch's comparison runs here so a
                // reconnect still sees the same heads.
                let reply = match act_as.as_deref() {
                    Some(name) => crate::daemon::sponsorship::watch(
                        self.router.asks(),
                        address.space.as_str(),
                        name,
                        &heads,
                    ),
                    None => crate::control::WaitReply::Idle,
                };
                let _ = write_line(&mut write_half, &Response::Wait(reply)).await;
                Flow::Next(reader, write_half)
            }
            (route, request) => {
                // The book is the one namer, and this funnel is where it
                // speaks: a Station answers with bare ids, and the identity
                // that owns the names decorates the reply on its way out. The
                // same seam names a just-provisioned agent — the Station has
                // no reach into the identity-scoped book, so it reports the
                // actor and the daemon authors the Card.
                let provisioned = match &request {
                    Request::AgentProvision { name } => Some(name.clone()),
                    _ => None,
                };
                let space = match &route {
                    ControlRoute::Orbit { address } | ControlRoute::World { address, .. } => {
                        Some(address.space.clone())
                    }
                    ControlRoute::Daemon => None,
                };
                let mut response = self
                    .router
                    .request_routed(route, &request, act_as.as_deref())
                    .await
                    .unwrap_or_else(|error| Response::err(format!("{error:#}")));
                if let Some(space) = &space {
                    if matches!(
                        response,
                        Response::Members { .. } | Response::Who { .. } | Response::Seeds { .. }
                    ) {
                        if let Ok(book) = self.router.book() {
                            book.decorate(space, &mut response);
                        }
                    }
                    if let (
                        Some(name),
                        Response::Ok {
                            message: Some(message),
                        },
                    ) = (&provisioned, &response)
                    {
                        // The reply's `actor …` line is part of the provision
                        // contract (see hosting's provision arm).
                        if let Some(actor) =
                            message.lines().find_map(|line| line.strip_prefix("actor "))
                        {
                            if let Ok(book) = self.router.book() {
                                book.name_agent(space, actor.trim(), name);
                            }
                        }
                        let actor = message.lines().find_map(|line| line.strip_prefix("actor "));
                        self.router
                            .asks()
                            .grant(space.as_str(), name, actor.map(str::trim));
                    }
                    if let (Request::Whoami, Some(name)) = (&request, act_as.as_deref()) {
                        // A named agent that is not a member is asking this
                        // identity to sponsor it — including the install-first
                        // case, where the seed does not exist yet and whoami
                        // is a Denied rather than a Whoami DTO. The ask is
                        // host-plane state so the client can notify; it is
                        // not a World signal.
                        match &mut response {
                            Response::Whoami(whoami) => {
                                crate::daemon::sponsorship::note_whoami(
                                    self.router.asks(),
                                    space.as_str(),
                                    name,
                                    whoami,
                                );
                            }
                            Response::Error { message, .. }
                                if crate::daemon::sponsorship::note_denied(
                                    self.router.asks(),
                                    space.as_str(),
                                    name,
                                ) =>
                            {
                                *message = format!(
                                    "you are not yet a member — sponsorship of '{name}' \
                                     has been requested from the person on this machine; \
                                     call wait (Work Watch) with the heads whoami gave you \
                                     ({message})"
                                );
                            }
                            _ => {}
                        }
                    }
                }
                let _ = write_line(&mut write_half, &response).await;
                Flow::Next(reader, write_half)
            }
        }
    }

    async fn stream_catalog(&self, mut write_half: tokio::io::WriteHalf<LocalStream>) {
        let mut doorbells = self.router.subscribe();
        let mut stop = self.stopping.subscribe();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                },
                doorbell = doorbells.recv() => match doorbell {
                    Ok(doorbell) => {
                        if write_line(&mut write_half, &doorbell).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // OrbitDoorbell has no catalog-wide reset frame. Closing
                        // makes the web adapter emit `lagged` and reconnect,
                        // which triggers its normal authoritative rebaseline.
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    async fn stream_space(
        &self,
        mut write_half: tokio::io::WriteHalf<LocalStream>,
        route: ControlRoute,
        since: u64,
    ) {
        let ControlRoute::Orbit { address } = &route else {
            return;
        };
        let resolved = match self.router.place_address(address).await {
            Ok(resolved) => resolved,
            Err(error) => {
                let _ = write_line(&mut write_half, &Response::err(format!("{error:#}"))).await;
                return;
            }
        };
        let mut subscription =
            match control::subscribe_routed(&resolved.home, since, Some(route)).await {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = write_line(&mut write_half, &Response::err(format!("{error:#}"))).await;
                    return;
                }
            };
        let mut stop = self.stopping.subscribe();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                },
                next = subscription.next() => match next {
                    Ok(Some(doorbell)) => {
                        if write_line(&mut write_half, &doorbell).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "proxied Space subscription ended");
                        break;
                    }
                }
            }
        }
    }

    async fn stream_live(
        &self,
        mut write_half: tokio::io::WriteHalf<LocalStream>,
        route: ControlRoute,
        world: String,
        body: Option<[u8; 16]>,
    ) {
        let ControlRoute::Orbit { address } = &route else {
            return;
        };
        let resolved = match self.router.place_address(address).await {
            Ok(resolved) => resolved,
            Err(error) => {
                let _ = write_line(&mut write_half, &Response::err(format!("{error:#}"))).await;
                return;
            }
        };
        let mut subscription =
            match control::subscribe_live_routed(&resolved.home, route, world, body).await {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = write_line(&mut write_half, &Response::err(format!("{error:#}"))).await;
                    return;
                }
            };
        let mut stop = self.stopping.subscribe();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                },
                next = subscription.next() => match next {
                    Ok(Some(view)) => {
                        if write_line(&mut write_half, &view).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "proxied Live subscription ended");
                        break;
                    }
                }
            }
        }
    }
}

/// The autonomous identity-scoped process supervisor.
pub struct Daemon {
    router: Arc<Router>,
    endpoint: Arc<Endpoint>,
    display: Arc<crate::display::DisplayRuntime>,
    relaunch_requested: Arc<AtomicBool>,
    /// The identity's own seed, held for the overlay endpoint the serve path
    /// stands up. The display custodian already carries it, so this is a
    /// second reference to material this process necessarily holds, not a new
    /// exposure.
    device_seed: [u8; 32],
}

impl Daemon {
    fn new(
        router: Arc<Router>,
        clients: world_interface::WorldClientRegistry,
        home: &Path,
        device_seed: &[u8; 32],
        profile: mechanics::kinship::ProfileId,
    ) -> Result<Self> {
        let display = Arc::new(crate::display::DisplayRuntime::open(
            &home.join("display"),
            router.clone(),
            clients,
            device_seed,
            profile,
            crate::config::Settings::load(Some(home)).display_port(),
        )?);
        Ok(Self {
            endpoint: Arc::new(Endpoint::new(router.clone(), display.clone())),
            display,
            router,
            relaunch_requested: Arc::new(AtomicBool::new(false)),
            device_seed: *device_seed,
        })
    }

    fn begin_stop(&self) {
        self.endpoint.begin_stop();
    }

    async fn serve(self: Arc<Self>, home: &Path) -> Result<()> {
        // Staging runs beside whatever else this daemon serves, including in
        // the reduced modes below: an embedded or display-less daemon is
        // still the resident updater for its installation. It is spawned
        // rather than selected on because it never completes on its own and
        // never fails the daemon — the worst a check can do is leave the
        // standing unchanged.
        let staging = self.spawn_staging();
        let world_upgrades = self.spawn_world_upgrades();

        // Display coordination is withheld from a daemon that does not own
        // the machine's posture: a guest in somebody's process (see
        // [`embed_in_host_process`]) and a daemon told `LAIT_DISPLAY=off`
        // (see [`display_hosting`]). The control socket stays, and the
        // display control-plane requests still answer (status reads state,
        // not the listener); only the LAN-facing HTTPS service is absent.
        if embedded() || !display_hosting() {
            let served = self.endpoint.clone().serve(home).await;
            Self::join_staging(staging).await;
            Self::join_world_upgrades(world_upgrades).await;
            return served;
        }
        // Take the port before committing to it, so "another daemon already holds
        // it" is separable from "our service broke". The port is fixed and bound
        // on `0.0.0.0`, which makes the coordinator a machine-wide singleton — so
        // on any machine that already runs a daemon, this is the *ordinary* case
        // rather than an exceptional one.
        //
        // It used to be fatal to the whole daemon, which meant a second identity
        // could not come up at all and a supervisor could not start a head for
        // it. The concern underneath that was right — never advertise an origin
        // receivers cannot reach — but the remedy was one size too large: the
        // answer to "cannot host displays" is to not host them and say so, not to
        // refuse to be a daemon.
        let display_listener = match crate::display::bind_display(&self.display.tls).await {
            // Kept, not dropped. Dropping it made this a guess rather than a
            // reservation: the port could be taken between here and the serving
            // bind, and that failure arrives on a path where the degradation below
            // does not run — so the daemon died on exactly the race this handles.
            Ok(listener) => listener,
            Err(error) if crate::display::is_port_taken(&error) => {
                tracing::warn!(
                    %error,
                    "another daemon on this machine holds the display port; \
                     serving without display coordination"
                );
                let served = self.endpoint.clone().serve(home).await;
                Self::join_staging(staging).await;
                return served;
            }
            Err(error) => {
                Self::join_staging(staging).await;
                return Err(error).context("serve daemon display HTTPS");
            }
        };
        // The same router the TCP path serves, reachable over the overlay:
        // addressed by endpoint id, no port, no inbound hole. A failure here
        // is a degradation — the LAN listener is already up — never a reason
        // for the daemon not to exist.
        let overlay_task = match comms::policy::Network::from_env() {
            Ok(network) => {
                let seed = self.device_seed;
                let state = crate::display::DisplayHttpState {
                    coordinator: self.display.coordinator.clone(),
                    pairing: self.display.pairing.clone(),
                };
                let overlay_stop = self.endpoint.subscribe_stop();
                let publication = crate::config::Settings::load(Some(home)).route_publication();
                let identity_home = home
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|| home.to_path_buf());
                Some(tokio::spawn(async move {
                    let transport = match comms::DefaultTransport::new(
                        &seed,
                        &network,
                        comms::Protocols {
                            framed: &[],
                            session: &[crate::display::overlay::DISPLAY_ALPN],
                        },
                    )
                    .await
                    {
                        Ok(transport) => std::sync::Arc::new(transport),
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                "display overlay endpoint could not bind;                                  serving the LAN listener only"
                            );
                            return;
                        }
                    };
                    // Say where this identity answers, when it has a label and
                    // a registry to say it to. Publication is evidence signed
                    // by this device; a refusal is logged and serving goes on,
                    // because a coordinator that cannot announce is degraded,
                    // not absent — LAN receivers never needed the registry.
                    if let Some((label, registry)) = publication {
                        let endpoint = comms::Transport::my_id(transport.as_ref())
                            .as_str()
                            .to_string();
                        let outcome = tokio::task::spawn_blocking(move || {
                            crate::display::publish_route(
                                &identity_home,
                                &label,
                                &registry,
                                &endpoint,
                            )
                        })
                        .await;
                        match outcome {
                            Ok(Ok(resolved)) => {
                                tracing::info!(label = %resolved.label.as_str(), "route published");
                            }
                            Ok(Err(error)) => {
                                tracing::warn!(%error, "route publication refused; serving anyway");
                            }
                            Err(error) => {
                                tracing::warn!(%error, "route publication task failed");
                            }
                        }
                    }
                    if let Err(error) = crate::display::overlay::serve_display_overlay(
                        transport,
                        crate::display::display_http_router(state),
                        overlay_stop,
                    )
                    .await
                    {
                        tracing::error!(%error, "display overlay service stopped");
                    }
                }))
            }
            Err(error) => {
                tracing::warn!(%error, "no overlay network policy; serving the LAN listener only");
                None
            }
        };
        let display = self.display.clone();
        let display_stop = self.endpoint.subscribe_stop();
        let endpoint = self.endpoint.clone();
        let mut display_service =
            Box::pin(async move { display.serve_on(display_listener, display_stop).await });
        let mut control_service = Box::pin(async move { endpoint.serve(home).await });
        let outcome = tokio::select! {
            endpoint_result = &mut control_service => {
                self.endpoint.begin_stop();
                if let Err(error) = display_service.await {
                    tracing::error!(%error, "display HTTPS service stopped during daemon shutdown");
                }
                endpoint_result
            }
            display_result = &mut display_service => {
                // A display bind or listener failure must not sit unnoticed
                // behind a healthy control socket: Astrolabe would advertise
                // an origin receivers cannot reach. Stop the sibling service
                // and fail startup as one daemon-owned unit.
                self.endpoint.begin_stop();
                let endpoint_result = control_service.await;
                endpoint_result?;
                match display_result {
                    Ok(()) => Err(anyhow!("display HTTPS service stopped unexpectedly")),
                    Err(error) => Err(error).context("serve daemon display HTTPS"),
                }
            }
        };
        // The overlay stops on the same watch every sibling uses; joining is
        // what keeps its endpoint from outliving the daemon that owns it.
        if let Some(overlay) = overlay_task {
            if let Err(error) = overlay.await {
                tracing::error!(%error, "display overlay task ended abnormally");
            }
        }
        Self::join_staging(staging).await;
        Self::join_world_upgrades(world_upgrades).await;
        outcome
    }

    /// Start the continuous staging watcher, when this daemon runs inside an
    /// installation of either shape — a stub-managed tree, or a macOS bundle
    /// staging beside the identity.
    ///
    /// `None` everywhere else — a developer's build tree and a standalone
    /// `lait` have nowhere to stage to, and inventing a root would drop a
    /// client tree beside somebody's `target/`. The watcher stops with the
    /// endpoint, on the same signal every other service here uses.
    fn spawn_staging(&self) -> Option<tokio::task::JoinHandle<()>> {
        let identity = self.router.catalog().identity().to_path_buf();
        let root = crate::update::watch::staging_root(&identity)?;
        if let Err(error) = std::fs::create_dir_all(&root) {
            // Said, not skipped: an unwritable staging path is a fact about
            // this machine, and silence here is a client that never updates
            // and never explains why.
            tracing::warn!(
                %error,
                root = %root.display(),
                "the staging root could not be created; this installation will not update"
            );
            return None;
        }
        let stop = self.endpoint.subscribe_stop();
        Some(tokio::spawn(crate::update::watch::serve(
            identity, root, stop,
        )))
    }

    /// Resume consented World updates independently of any client connection.
    /// One turn performs at most one fetch or one bounded product migration
    /// step, so progress never monopolizes the host reactor or a Station.
    fn spawn_world_upgrades(&self) -> tokio::task::JoinHandle<()> {
        let router = self.router.clone();
        let worlds = crate::serve::head::worlds_root(router.catalog().identity());
        let stop = self.endpoint.subscribe_stop();
        let relaunch = GenerationRelaunch {
            requested: self.relaunch_requested.clone(),
            endpoint: self.endpoint.clone(),
        };
        tokio::spawn(serve_world_upgrades(router, worlds, stop, relaunch))
    }

    /// Wait for the staging watcher to notice the stop signal, bounded so a
    /// check in flight can never hold a shutdown open.
    async fn join_staging(staging: Option<tokio::task::JoinHandle<()>>) {
        let Some(staging) = staging else {
            return;
        };
        match tokio::time::timeout(Duration::from_secs(5), staging).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "the staging watcher ended abnormally"),
            Err(_) => tracing::debug!(
                "the staging watcher did not finish in time; leaving it to the process exit"
            ),
        }
    }

    async fn join_world_upgrades(upgrades: tokio::task::JoinHandle<()>) {
        match tokio::time::timeout(Duration::from_secs(5), upgrades).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "the World upgrade worker ended abnormally"),
            Err(_) => tracing::debug!(
                "the World upgrade worker did not finish in time; leaving it to process exit"
            ),
        }
    }
}

#[derive(Clone)]
struct GenerationRelaunch {
    requested: Arc<AtomicBool>,
    endpoint: Arc<Endpoint>,
}

impl GenerationRelaunch {
    fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.endpoint.begin_stop();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorldUpgradeAdvance {
    Progressed,
    Relaunch,
}

async fn serve_world_upgrades(
    router: Arc<Router>,
    worlds: PathBuf,
    mut stop: tokio::sync::watch::Receiver<bool>,
    relaunch: GenerationRelaunch,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            _ = interval.tick() => {
                match advance_one_world_upgrade(router.clone(), worlds.clone()).await {
                    Ok(WorldUpgradeAdvance::Relaunch) => {
                        tracing::info!("World release staged; crossing the daemon generation boundary");
                        relaunch.request();
                        break;
                    }
                    Ok(WorldUpgradeAdvance::Progressed) => {}
                    Err(error) => tracing::warn!(%error, "could not advance a consented World update"),
                }
            }
        }
    }
}

async fn advance_one_world_upgrade(
    router: Arc<Router>,
    worlds: PathBuf,
) -> Result<WorldUpgradeAdvance> {
    let world_ids: Vec<_> = router.lifecycle_world_ids().cloned().collect();
    for world in world_ids {
        let world_name = world.as_str().to_owned();
        let worlds_for_read = worlds.clone();
        let job = match router
            .run_blocking(move || crate::update::consent::load(&worlds_for_read, &world_name))
            .await
        {
            Ok(Some(job)) if !job.terminal() => job,
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(world = %world, %error, "cannot read World update consent");
                continue;
            }
        };
        return advance_world_upgrade_job(router, worlds, world, job).await;
    }
    Ok(WorldUpgradeAdvance::Progressed)
}

async fn advance_world_upgrade_job(
    router: Arc<Router>,
    worlds: PathBuf,
    world: replica::body::WorldId,
    mut job: crate::update::consent::Job,
) -> Result<WorldUpgradeAdvance> {
    use crate::update::consent::Phase;

    // `current.json` can move while this daemon is running, but every Runtime
    // Catalog and client adapter in this process is pinned to the release it
    // launched. Only a fresh daemon may interpret the new descriptor. The
    // durable phase is written before the old generation drains; the new
    // generation recognizes its own release here and begins migrations from
    // the first Space under the new implementation.
    if job.phase == Phase::Relaunching {
        let Some(staged) = job.staged_version.as_deref() else {
            anyhow::bail!("World update entered relaunching without a staged release")
        };
        if router.world_release_version(&world) != Some(staged) {
            return Ok(WorldUpgradeAdvance::Relaunch);
        }
        job.phase = Phase::Migrating;
        job.current_orbit = None;
        job.after_orbit = None;
        job.completed_spaces = 0;
        job.total_spaces = 0;
        job.completed_records = 0;
        job.remaining_records = None;
        job.message = Some(format!(
            "release {staged} is running; verifying every Space"
        ));
        job.updated_at = mechanics::wallclock::now_secs();
        let worlds_for_save = worlds.clone();
        router
            .run_blocking(move || crate::update::consent::save(&worlds_for_save, &job))
            .await?;
        return Ok(WorldUpgradeAdvance::Progressed);
    }

    if job.staged_version.is_none() {
        let worlds_for_fetch = worlds.clone();
        let world_name = world.as_str().to_owned();
        let operation = job.operation;
        let running_version = router.world_release_version(&world).map(str::to_owned);
        job = router
            .run_blocking(move || {
                let Some(mut current) =
                    crate::update::consent::load(&worlds_for_fetch, &world_name)?
                else {
                    anyhow::bail!("World update consent disappeared")
                };
                if current.operation != operation || current.terminal() {
                    return Ok(current);
                }
                current.phase = Phase::Fetching;
                current.message = None;
                current.updated_at = mechanics::wallclock::now_secs();
                crate::update::consent::save(&worlds_for_fetch, &current)?;
                let outcome = crate::update::world::check(
                    &world_name,
                    &worlds_for_fetch,
                    crate::update::feed::Channel::current(),
                );
                match outcome {
                    Ok(outcome) => {
                        crate::update::world::note(
                            &worlds_for_fetch,
                            &world_name,
                            &outcome,
                            mechanics::wallclock::now_secs(),
                        );
                        match outcome {
                            crate::update::world::Outcome::Staged { version }
                            | crate::update::world::Outcome::Current { version } => {
                                current.staged_version = Some(version.clone());
                                if running_version.as_deref() == Some(version.as_str()) {
                                    current.phase = Phase::Migrating;
                                } else {
                                    current.phase = Phase::Relaunching;
                                    current.message = Some(format!(
                                        "release {version} selected; relaunching its daemon generation"
                                    ));
                                }
                            }
                            crate::update::world::Outcome::Unmet { version, why } => {
                                current.phase = Phase::Refused;
                                current.message =
                                    Some(format!("{version} requires {}", why.join(", ")));
                            }
                            crate::update::world::Outcome::NothingPublished { version } => {
                                current.phase = Phase::Refused;
                                current.message = Some(format!(
                                    "release {version} carries no bundle for this World"
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            world = %world_name,
                            %error,
                            "World bundle verification will retry"
                        );
                        current.phase = Phase::Waiting;
                        current.message = Some("Waiting to retry World bundle verification".into());
                    }
                }
                current.updated_at = mechanics::wallclock::now_secs();
                crate::update::consent::save(&worlds_for_fetch, &current)?;
                Ok(current)
            })
            .await?;
        return Ok(if job.phase == Phase::Relaunching {
            WorldUpgradeAdvance::Relaunch
        } else {
            WorldUpgradeAdvance::Progressed
        });
    }

    let bindings = router.visible_orbit_ids_blocking().await?;
    job.total_spaces = u64::try_from(bindings.len()).unwrap_or(u64::MAX);
    let had_stale_orbit = job
        .current_orbit
        .as_ref()
        .is_some_and(|current| !bindings.contains(current));
    let orbit = reconcile_upgrade_orbit(&mut job, &bindings);
    if had_stale_orbit {
        tracing::debug!(
            world = %world,
            "reconciling a lifecycle cursor whose Space is no longer bound"
        );
    }
    let Some(orbit) = orbit else {
        job.phase = Phase::Verified;
        job.message = Some("bundle staged and every visible Space verified".into());
        job.updated_at = mechanics::wallclock::now_secs();
        let worlds_for_save = worlds.clone();
        router
            .run_blocking(move || crate::update::consent::save(&worlds_for_save, &job))
            .await?;
        return Ok(WorldUpgradeAdvance::Progressed);
    };
    if job.current_orbit.is_none() {
        job.current_orbit = Some(orbit.clone());
        job.phase = Phase::Migrating;
        job.message = None;
        job.updated_at = mechanics::wallclock::now_secs();
        let worlds_for_save = worlds.clone();
        let staged = job.clone();
        router
            .run_blocking(move || crate::update::consent::save(&worlds_for_save, &staged))
            .await?;
    }

    let step = match router
        .advance_world_upgrade(&orbit, world.clone(), job.operation)
        .await
    {
        Ok(step) => step,
        Err(error) => {
            tracing::warn!(
                world = %world,
                orbit = %orbit,
                %error,
                "Space lifecycle step will retry"
            );
            job.phase = Phase::Waiting;
            job.message = Some("Waiting to retry the bounded Space lifecycle step".into());
            job.updated_at = mechanics::wallclock::now_secs();
            let worlds_for_save = worlds.clone();
            router
                .run_blocking(move || crate::update::consent::save(&worlds_for_save, &job))
                .await?;
            return Ok(WorldUpgradeAdvance::Progressed);
        }
    };
    match step {
        crate::orbital::WorldUpgradeStep::Pending {
            completed,
            remaining,
        } => {
            job.phase = Phase::Migrating;
            job.completed_records = completed;
            job.remaining_records = remaining;
            job.message = Some(format!("Space {orbit} migration is in progress"));
        }
        crate::orbital::WorldUpgradeStep::Building => {
            job.phase = Phase::Waiting;
            job.message = Some(format!(
                "Space {orbit} frozen migration source is rebuilding"
            ));
        }
        crate::orbital::WorldUpgradeStep::Capacity => {
            job.phase = Phase::Waiting;
            job.message = Some(format!(
                "Space {orbit} frozen migration source awaits read capacity"
            ));
        }
        crate::orbital::WorldUpgradeStep::Current
        | crate::orbital::WorldUpgradeStep::Unbound
        | crate::orbital::WorldUpgradeStep::Verified => {
            job.after_orbit = Some(orbit);
            job.current_orbit = None;
            job.completed_spaces = job.completed_spaces.saturating_add(1);
            job.completed_records = 0;
            job.remaining_records = None;
            job.phase = Phase::Migrating;
            job.message = None;
        }
        crate::orbital::WorldUpgradeStep::Unsupported { reason } => {
            job.phase = Phase::Refused;
            job.message = Some(reason);
        }
    }
    job.updated_at = mechanics::wallclock::now_secs();
    let worlds_for_save = worlds;
    router
        .run_blocking(move || crate::update::consent::save(&worlds_for_save, &job))
        .await?;
    Ok(WorldUpgradeAdvance::Progressed)
}

/// Reconcile a persisted cursor with the current exact set of World-bound
/// Spaces. A removed/unbound Space is ordinary restart drift, not a semantic
/// migration refusal; resume at the next canonical binding.
fn reconcile_upgrade_orbit(
    job: &mut crate::update::consent::Job,
    bindings: &[String],
) -> Option<String> {
    if job
        .current_orbit
        .as_ref()
        .is_some_and(|current| !bindings.contains(current))
    {
        job.current_orbit = None;
    }
    job.current_orbit.clone().or_else(|| {
        bindings
            .iter()
            .find(|orbit| job.after_orbit.as_ref().is_none_or(|after| *orbit > after))
            .cloned()
    })
}

/// Joinable ownership of the process endpoint and its process-wide lock.
pub(crate) struct Runner {
    home: PathBuf,
    daemon: Arc<Daemon>,
    _lock: DaemonLock,
}

#[derive(Clone)]
pub(crate) struct Stop {
    daemon: Weak<Daemon>,
}

impl Stop {
    pub(crate) fn stop(&self) {
        if let Some(daemon) = self.daemon.upgrade() {
            daemon.begin_stop();
        }
    }
}

impl Runner {
    /// `device_seed` is this identity's own seed, threaded through as a value
    /// rather than re-read here: the daemon is the identity singleton, and a
    /// second read is a second chance to disagree about which identity is
    /// running.
    pub(crate) fn start(
        home: PathBuf,
        router: Arc<Router>,
        clients: world_interface::WorldClientRegistry,
        device_seed: [u8; 32],
        profile: mechanics::kinship::ProfileId,
    ) -> Result<Self> {
        let lock = acquire_daemon_lock(&home)?;
        let daemon = Arc::new(Daemon::new(router, clients, &home, &device_seed, profile)?);
        Ok(Self {
            home,
            daemon,
            _lock: lock,
        })
    }

    pub(crate) fn stop_handle(&self) -> Stop {
        Stop {
            daemon: Arc::downgrade(&self.daemon),
        }
    }

    fn relaunch_requested(&self) -> Arc<AtomicBool> {
        self.daemon.relaunch_requested.clone()
    }

    pub(crate) async fn run(self) -> Result<()> {
        let serve_result = self.daemon.clone().serve(&self.home).await;
        Phase::DrainingPlacements.enter();
        let shutdown_result = self
            .daemon
            .router
            .shutdown()
            .await
            .context("drain Station placements");
        Phase::Done.enter();
        #[cfg(unix)]
        let _ = std::fs::remove_file(crate::config::socket_path(&self.home));
        serve_result?;
        shutdown_result
    }
}

/// Run the always-on Lait daemon for one identity.
///
/// `selection` names which identity, as a value rather than through the process
/// environment: the daemon is spawned with `--home`, and turning that flag back
/// into an env var made the choice a property of the process instead of of the
/// call.
pub async fn run_lait_daemon(
    packages: WorldPackages,
    clients: world_interface::WorldClientRegistry,
    selection: crate::config::Selection,
) -> Result<()> {
    let identity = selection.identity_dir()?;
    // The daemon is the identity singleton, so the seed is minted here, at
    // boot, deliberately — never as a side effect of a later write (the
    // address book's author path is load-only by design).
    std::fs::create_dir_all(&identity)?;
    let device_seed = crate::config::load_or_create_identity(&identity)?;
    // The identity's address, derived once beside its seed for the same
    // reason the seed is: a value threaded down, never re-read to disagree.
    let profile = crate::config::identity_profile(&identity)?;
    let config_root = crate::config::config_root()?;
    let self_contained = selection.self_contained();
    let agents_base = crate::registry::agents_base(&config_root);
    let home = selection.daemon_home()?;
    let router = Arc::new(Router::new(
        Catalog::new(identity, agents_base, self_contained),
        packages,
    ));
    let runner = Runner::start(home, router, clients, device_seed, profile)?;
    let relaunch_requested = runner.relaunch_requested();
    let stop = runner.stop_handle();
    let signal = tokio::spawn(async move {
        shutdown_signal().await;
        watchdog();
        stop.stop();
        // A second signal is an instruction not to wait for the ladder at all.
        shutdown_signal().await;
        tracing::warn!("second shutdown signal — leaving without finishing the drain");
        exit_now();
    });
    let result = runner.run().await;
    signal.abort();
    let _ = signal.await;
    // The listener is gone, so from here tokio's still-installed handler would
    // swallow SIGTERM rather than end us. Whatever remains — the runtime's own
    // teardown, a blocking task that has not noticed — stays interruptible the
    // ordinary way.
    crate::process::restore_default_termination_signals();
    if result.is_ok() && relaunch_requested.load(Ordering::Acquire) {
        let executable =
            std::env::current_exe().context("locate daemon executable for relaunch")?;
        let home = selection.daemon_home()?;
        let log = std::fs::File::create(crate::host_client::daemon_log_path(&home)).ok();
        let identity = selection.self_contained_home();
        crate::daemon_spawn::spawn(&executable, log, identity.as_deref())
            .context("spawn the next World daemon generation")?
            .reap();
    }
    result
}

/// How far shutdown got, published for the one observer that can still read it.
///
/// A daemon wedged in its drain is silent by construction: the rung it is stuck
/// on is the rung that never returns, so it never logs. That left a real hang —
/// alive, idle, every worker parked, deaf to SIGTERM — with no evidence at all
/// beyond a stack sample of a stripped release binary. This is the breadcrumb
/// that turns the next one into a one-line diagnosis: the watchdog reads it from
/// its own thread and names the stage it interrupted.
///
/// An atom rather than anything richer because the reader is a bare OS thread
/// running while the runtime may be unable to schedule anything at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Phase {
    Serving = 0,
    DrainingConnections = 1,
    DrainingPlacements = 2,
    Done = 3,
}

static SHUTDOWN_PHASE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(Phase::Serving as u8);

impl Phase {
    fn enter(self) {
        SHUTDOWN_PHASE.store(self as u8, std::sync::atomic::Ordering::Relaxed);
        // Arm the hard stop on the *first* sign of shutdown, whatever started
        // it. A signal is only one of the ways this daemon stops: a head taking
        // over sends `Request::Stop`, and the accept loop can break on its own.
        // Those paths never reach the signal task, so arming only there left the
        // commonest wedge uncovered — and once the signal task is aborted the
        // registered SIGTERM handler stays installed and inert, so a process
        // wedged after that point cannot be signalled down at all.
        if matches!(self, Phase::DrainingConnections | Phase::DrainingPlacements) {
            watchdog();
        }
    }

    fn current() -> &'static str {
        match SHUTDOWN_PHASE.load(std::sync::atomic::Ordering::Relaxed) {
            0 => "still serving — the stop never reached the accept loop",
            1 => "draining control connections",
            2 => "draining Station placements (Orbit teardown, peer goodbye, transport close)",
            _ => "finished draining — stuck after the last rung",
        }
    }
}

/// How long the graceful drain gets before the hard stop takes over.
///
/// Well above what a real shutdown costs (a node with a placed Station and a
/// live peer measures in single-digit seconds), because the drain is worth
/// finishing: it is what sends the signed dormancy beacon that stops peers
/// treating this node as reachable. The deadline is the backstop for a rung
/// that never returns, not a schedule the ordinary path is meant to meet.
const DEADLINE: Duration = Duration::from_secs(30);

/// Arm the hard stop behind the graceful one, on a plain OS thread.
///
/// Deliberately not a runtime task. Shutdown is a ladder of drains, and the
/// failure this guards against — a rung that never returns, a worker pinned by
/// a blocking call, a runtime that cannot schedule — is precisely the state in
/// which a `tokio::time::timeout` would never be polled. A thread parked in
/// `sleep` is answerable to nobody's scheduler.
///
/// Armed from more than one place — a signal, and the drain itself — so the
/// deadline is the *first* arming and later calls are no-ops. Re-arming would
/// let a shutdown that crawls from rung to rung extend its own deadline forever.
///
/// `LAIT_SHUTDOWN_DEADLINE_SECS` overrides it; `0` disables the hard stop, for
/// anyone who would rather have a hung daemon to debug than a clean exit.
fn watchdog() {
    static ARMED: std::sync::Once = std::sync::Once::new();
    ARMED.call_once(arm_watchdog);
}

/// Whether this daemon runs as a library inside somebody else's process.
static EMBEDDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// An embedder's standing declaration: this daemon is a guest in a process —
/// and on a device — it does not own. Called once before the daemon starts; a
/// value rather than an environment mutation, for the same reason
/// `run_lait_daemon` takes its selection as one.
///
/// Two consequences, both sides of the same fact:
///
/// - **The exit watchdog never arms.** Its whole action is `exit()`, and a
///   library must never exit its host — on iOS the daemon runs as a task
///   inside the application, and the deadline would take the interface down
///   with the drain it was policing. `LAIT_SHUTDOWN_DEADLINE_SECS=0` stays
///   the CLI's spelling of that half.
/// - **No machine-scoped listener is hosted.** Display coordination binds a
///   well-known LAN-facing port that receivers discover; that is the desktop
///   identity daemon's posture. A guest daemon must neither claim that port
///   out from under the machine's real daemon nor open its host device to
///   the LAN.
pub fn embed_in_host_process() {
    EMBEDDED.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn embedded() -> bool {
    EMBEDDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether this daemon hosts the machine's display coordinator.
///
/// On by default: the identity daemon owns the machine's display posture.
/// `LAIT_DISPLAY=off` withholds it — the coordinator binds one well-known,
/// machine-scoped port, and a daemon that is not *the* machine's daemon must
/// neither race the real one for that port nor die because it lost. The test
/// suite is the standing case: it runs many daemons on one machine in
/// parallel, and a fixed port makes them mutually exclusive — every spawned
/// test daemon says `off` and the one suite that exercises receivers leaves
/// it hosting.
fn display_hosting() -> bool {
    !matches!(
        std::env::var("LAIT_DISPLAY").as_deref(),
        Ok("off" | "0" | "false")
    )
}

fn arm_watchdog() {
    if embedded() {
        return;
    }
    let deadline = std::env::var("LAIT_SHUTDOWN_DEADLINE_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map_or(DEADLINE, Duration::from_secs);
    if deadline.is_zero() {
        return;
    }
    // A watchdog we could not start is not worth refusing to shut down over.
    let _ = std::thread::Builder::new()
        .name("lait-shutdown-watchdog".into())
        .spawn(move || {
            std::thread::sleep(deadline);
            tracing::error!(
                seconds = deadline.as_secs(),
                stage = Phase::current(),
                "graceful shutdown did not finish within its deadline — leaving anyway"
            );
            exit_now();
        });
}

/// Leave now, skipping every remaining drain and destructor.
///
/// Safe to do at all only because durability never depended on this path: the
/// journal is fsync/rename disciplined and crash-tested against a mid-commit
/// abort, the store lock is an `flock` the kernel drops on exit, and the next
/// daemon unlinks a stale socket before it binds. What is genuinely lost is the
/// courtesy — the dormancy beacon peers use to stop calling — which is why this
/// is the deadline's job and not the shutdown's.
///
/// Exit status `0`: the process was asked to stop and it stopped. The daemon's
/// non-zero codes are a startup contract read by the head that spawned it, and
/// a shutdown that took too long is not a failure to start.
#[allow(
    clippy::exit,
    reason = "the deadline behind graceful shutdown: its whole purpose is to leave when the ordinary path will not"
)]
fn exit_now() -> ! {
    std::process::exit(0)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let terminate = async {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                signal.recv().await;
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

/// Where a proxied exchange broke, which decides whether anything may still be
/// said about it.
enum ProxyFailure {
    /// Nothing has reached the client yet, so a typed refusal is still a
    /// refusal.
    BeforeHeader(String),
    /// The client has already been told how many bytes follow. Anything written
    /// now *is* those bytes.
    AfterHeader,
}

/// Forward one content call to the attached process that owns the Station, and
/// its answer back.
///
/// Byte for byte and bounded at every step: the request body is pumped across
/// in pieces rather than collected, the answer's header is read under the
/// control-frame bound, and the answer's body is exactly as long as that header
/// declared. Nothing here decodes the content — the router is not a party to
/// what the bytes are.
///
/// The failure type carries *when* rather than only *what*, because after the
/// header has been forwarded there is nothing safe to say. The client is inside
/// `read_exact` for the declared length; a JSON error appended there is not an
/// error message, it is the first bytes of the file, and nothing on the far side
/// can tell the difference.
async fn proxy_content(
    home: &std::path::Path,
    mut reader: BufReader<tokio::io::ReadHalf<LocalStream>>,
    write_half: &mut tokio::io::WriteHalf<LocalStream>,
    request: &control::ContentClientRequest,
) -> Result<(), ProxyFailure> {
    use tokio::io::AsyncReadExt;

    fn before(what: &str) -> impl Fn(String) -> ProxyFailure + '_ {
        move |e| ProxyFailure::BeforeHeader(format!("{what}: {e}"))
    }

    let name =
        control::control_name(home).map_err(|e| ProxyFailure::BeforeHeader(format!("{e:#}")))?;
    let upstream = LocalStream::connect(name)
        .await
        .map_err(|e| before("connect to the attached Space process")(e.to_string()))?;
    let (upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let mut header = serde_json::to_string(request)
        .map_err(|e| before("encode content request")(e.to_string()))?;
    header.push('\n');
    upstream_write
        .write_all(header.as_bytes())
        .await
        .map_err(|e| before("write content request")(e.to_string()))?;
    upstream_write
        .flush()
        .await
        .map_err(|e| before("write content request")(e.to_string()))?;

    let mut left = request.body_len;
    let mut piece = vec![0u8; PROXY_PIECE_BYTES];
    while left > 0 {
        let want = left.min(PROXY_PIECE_BYTES as u64) as usize;
        reader
            .read_exact(&mut piece[..want])
            .await
            .map_err(|e| before("read content body")(e.to_string()))?;
        upstream_write
            .write_all(&piece[..want])
            .await
            .map_err(|e| before("forward content body")(e.to_string()))?;
        left -= want as u64;
    }
    upstream_write
        .flush()
        .await
        .map_err(|e| before("forward content body")(e.to_string()))?;

    let mut upstream = BufReader::new(upstream_read);
    let mut line = String::new();
    {
        let mut bounded = (&mut upstream).take(control::MAX_CONTROL_FRAME_BYTES);
        bounded
            .read_line(&mut line)
            .await
            .map_err(|e| before("read the attached answer")(e.to_string()))?;
    }
    if line.trim().is_empty() {
        return Err(ProxyFailure::BeforeHeader(
            "the attached Space process closed without answering".into(),
        ));
    }
    let reply: control::ContentReply = serde_json::from_str(line.trim())
        .map_err(|e| before("decode the attached answer")(e.to_string()))?;
    // Everything that could be checked about the answer is checked *before* the
    // header goes out, so that a bad answer is still a refusal rather than a
    // truncated file.
    if let control::ContentReply::ContentStream { len } = reply {
        if len > runtime::plane::freight::content::MAX_RANGE_BYTES as u64 {
            return Err(ProxyFailure::BeforeHeader(
                "the attached Space process offered an answer past the range bound".into(),
            ));
        }
    }
    write_half
        .write_all(line.as_bytes())
        .await
        .map_err(|e| before("write the answer")(e.to_string()))?;

    // Past this point the client is counting bytes, so every failure is silent
    // by necessity: the connection ends and the short read is the report.
    if let control::ContentReply::ContentStream { len } = reply {
        let mut left = len;
        while left > 0 {
            let want = left.min(PROXY_PIECE_BYTES as u64) as usize;
            if upstream.read_exact(&mut piece[..want]).await.is_err() {
                return Err(ProxyFailure::AfterHeader);
            }
            if write_half.write_all(&piece[..want]).await.is_err() {
                return Err(ProxyFailure::AfterHeader);
            }
            left -= want as u64;
        }
    }
    if write_half.flush().await.is_err() {
        return Err(ProxyFailure::AfterHeader);
    }
    Ok(())
}

/// How much the proxy moves at a time. One chunk, so a forward never holds more
/// than the sealer on the other end would.
const PROXY_PIECE_BYTES: usize = 256 * 1024;

async fn write_line<T: serde::Serialize>(
    write_half: &mut tokio::io::WriteHalf<LocalStream>,
    value: &T,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value)
        .unwrap_or_else(|_| "{\"kind\":\"error\",\"message\":\"encode failure\"}".into());
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await
}

/// Factory-injected constructor used by lifecycle tests.
#[cfg(test)]
pub(crate) fn runner_with_factory(
    home: PathBuf,
    catalog: Catalog,
    factory: Arc<dyn TransportFactory>,
) -> Result<Runner> {
    Runner::start(
        home,
        Arc::new(Router::with_factory(
            catalog,
            factory,
            crate::world::packages(),
        )),
        crate::world::client_packages().clone(),
        [0x5a; 32],
        test_profile(),
    )
}

/// A fixed, valid profile for lifecycle tests — the derivation the daemon
/// itself uses, over throwaway seeds.
#[cfg(test)]
pub(crate) fn test_profile() -> mechanics::kinship::ProfileId {
    correspondence::plane::ReachPlane::profile_for(&[[0x5a; 32], [0x5b; 32]])
        .expect("derive test profile")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::orbits::{Entry, Origin};
    use comms::mem::MemNet;
    use comms::policy::Network;
    use comms::Transport;

    static HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn restart_drift_skips_an_unbound_space_and_keeps_operation_progress() {
        let root = tempfile::tempdir().expect("temp root");
        let mut job = crate::update::consent::enqueue(
            root.path(),
            "lait.test.lifecycle",
            mechanics::wallclock::now_secs(),
        )
        .expect("consent");
        let operation = job.operation;
        job.phase = crate::update::consent::Phase::Waiting;
        job.after_orbit = Some("space-a".into());
        job.current_orbit = Some("space-b".into());
        job.completed_records = 256;

        let next = reconcile_upgrade_orbit(
            &mut job,
            &["space-a".into(), "space-c".into(), "space-d".into()],
        );
        assert_eq!(next.as_deref(), Some("space-c"));
        assert_eq!(job.current_orbit, None);
        assert_eq!(job.operation, operation);
        assert_eq!(job.completed_records, 256);
    }

    struct MemFactory(MemNet);

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _protocols: comms::Protocols<'_>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(
                self.0
                    .peer(mechanics::actor::device_from_seed(identity_seed)),
            ))
        }
    }

    fn formed_directory(
        tag: &str,
        seed: &[u8; 32],
    ) -> (PathBuf, PathBuf, Catalog, crate::orbits::ResolvedOrbit) {
        let n = HOME_COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("lait-host-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("orbit");
        let daemon_home = base.join("daemon");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&daemon_home).unwrap();
        let (mechanics, _) =
            crate::orbital::form_space(&crate::world::packages(), &home, seed, "Host Test")
                .unwrap();
        std::fs::write(
            home.join("secret.key"),
            data_encoding::HEXLOWER.encode(seed),
        )
        .unwrap();
        let id = super::super::LocalOrbitId::for_store(&home);
        let directory = Catalog::with_entries(
            home.clone(),
            home.join("agents"),
            false,
            vec![Entry {
                space: mechanics.space().as_str().to_string(),
                name: "Host Test".into(),
                path: home.to_string_lossy().to_string(),
                origin: Origin::Founded,
                host_nick: String::new(),
                last_opened: 1,
            }],
        );
        let resolved = directory.resolve(id.as_str()).unwrap();
        (base, daemon_home, directory, resolved)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_host_endpoint_places_routes_streams_and_drains_an_orbit() {
        // A test daemon is not the machine's daemon — see [`display_hosting`].
        // Hosting here would race every parallel test (and any real daemon on
        // this machine) for the coordinator's one well-known port.
        std::env::set_var("LAIT_DISPLAY", "off");
        let seed = [211; 32];
        let (base, daemon_home, directory, resolved) = formed_directory("route", &seed);
        let orbit_home = resolved.home.clone();
        let runner = runner_with_factory(
            daemon_home.clone(),
            directory,
            Arc::new(MemFactory(MemNet::new())),
        )
        .unwrap();
        let completion = tokio::spawn(runner.run());
        let client = Client::at(daemon_home.clone());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !matches!(client.probe().await, control::Probe::Healthy { .. }) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "host endpoint did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut catalog = client.subscribe_catalog().await.unwrap();
        let route = control::station_route(resolved.address.clone());
        assert!(matches!(
            client
                .request_if_running(route.clone(), &Request::Status)
                .await
                .unwrap(),
            Response::Error { .. }
        ));
        drop(
            acquire_daemon_lock(&orbit_home)
                .expect("a passive status probe must not activate the Orbit"),
        );
        assert!(matches!(
            client
                .request_if_running(route.clone(), &Request::WorldsActive)
                .await
                .unwrap(),
            Response::Error { .. }
        ));
        drop(
            acquire_daemon_lock(&orbit_home)
                .expect("a passive World probe must not activate the Orbit"),
        );
        assert!(matches!(
            client
                .request(route.clone(), &Request::Status, None)
                .await
                .unwrap(),
            Response::Status(_)
        ));
        assert!(matches!(
            client
                .request_if_running(route, &Request::WorldsActive)
                .await
                .unwrap(),
            Response::Worlds { .. }
        ));
        let call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList {
            page: issues::contract::PageRequest::default(),
        })
        .unwrap();
        let world_route = ControlRoute::World {
            address: resolved.address,
            world: call.world().as_str().to_string(),
        };
        let reply = client
            .call_world(world_route, call.clone(), None)
            .await
            .unwrap();
        let value = issues_app::decode_reply(&call, reply).unwrap();
        assert!(matches!(
            serde_json::from_value::<issues_app::IssuesResponse>(value).unwrap(),
            issues_app::IssuesResponse::Projects { .. }
        ));
        let ring = tokio::time::timeout(Duration::from_secs(2), catalog.next())
            .await
            .expect("catalog doorbell")
            .unwrap()
            .expect("catalog stream remains open");
        assert_eq!(
            ring.orbit,
            super::super::LocalOrbitId::for_store(&orbit_home)
        );
        assert!(
            acquire_daemon_lock(&daemon_home).is_err(),
            "host process lease remains held"
        );
        assert!(
            acquire_daemon_lock(&orbit_home).is_err(),
            "placed Station retains its Orbit lease"
        );

        client
            .request(ControlRoute::Daemon, &Request::Stop, None)
            .await
            .unwrap();
        completion.await.unwrap().unwrap();
        drop(acquire_daemon_lock(&daemon_home).expect("host lease released"));
        drop(acquire_daemon_lock(&orbit_home).expect("Orbit returned to vacancy"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_rejects_a_stale_space_expectation_before_placement() {
        // See the sibling test: a test daemon never hosts displays.
        std::env::set_var("LAIT_DISPLAY", "off");
        let seed = [212; 32];
        let (base, daemon_home, directory, mut resolved) = formed_directory("scope", &seed);
        let orbit_home = resolved.home.clone();
        resolved.address.space = mechanics::ids::SpaceId::from_digest([99; 16]);
        let runner = runner_with_factory(
            daemon_home.clone(),
            directory,
            Arc::new(MemFactory(MemNet::new())),
        )
        .unwrap();
        let completion = tokio::spawn(runner.run());
        let client = Client::at(daemon_home);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !matches!(client.probe().await, control::Probe::Healthy { .. }) {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let response = client
            .request(
                ControlRoute::Orbit {
                    address: resolved.address,
                },
                &Request::Status,
                None,
            )
            .await
            .unwrap();
        assert!(matches!(response, Response::Error { .. }));
        drop(
            acquire_daemon_lock(&orbit_home)
                .expect("a rejected address must not activate the Orbit"),
        );

        client
            .request(ControlRoute::Daemon, &Request::Stop, None)
            .await
            .unwrap();
        completion.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(base);
    }
}
