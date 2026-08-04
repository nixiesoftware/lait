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
        issue: Option<String>,
    ) -> Result<control::LiveSubscription> {
        control::subscribe_live_routed(&self.home, route, issue).await
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
    stopping: tokio::sync::watch::Sender<bool>,
}

impl Listener {
    pub(crate) fn new(router: Arc<Router>) -> Self {
        Self {
            router,
            stopping: tokio::sync::watch::channel(false).0,
        }
    }

    pub(crate) fn begin_stop(&self) {
        self.stopping.send_replace(true);
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
            let response = match (&route, &request) {
                (
                    ControlRoute::Orbit { .. },
                    Request::Status | Request::Id | Request::ConfigReload,
                ) => self
                    .router
                    .request_running(route, &request)
                    .await
                    .unwrap_or_else(|error| Response::err(format!("{error:#}"))),
                _ => Response::err(
                    "passive dispatch is only available for status, id, or config reload through \
                     an explicit Space route",
                ),
            };
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
            (ControlRoute::Daemon, request) => {
                let response = crate::orbits::bootstrap::dispatch(&self.router, request)
                    .await
                    .unwrap_or_else(|| Response::err("request has no daemon-scoped handler"));
                let _ = write_line(&mut write_half, &response).await;
                Flow::Next(reader, write_half)
            }
            (route @ ControlRoute::Orbit { .. }, Request::Subscribe { since }) => {
                self.stream_space(write_half, route, since).await;
                Flow::Close
            }
            (route @ ControlRoute::Orbit { .. }, Request::LiveSubscribe { issue }) => {
                self.stream_live(write_half, route, issue).await;
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
            (route, request) => {
                let response = self
                    .router
                    .request_routed(route, &request, act_as.as_deref())
                    .await
                    .unwrap_or_else(|error| Response::err(format!("{error:#}")));
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
        issue: Option<String>,
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
            match control::subscribe_live_routed(&resolved.home, route, issue).await {
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
}

impl Daemon {
    fn new(router: Arc<Router>) -> Self {
        Self {
            endpoint: Arc::new(Endpoint::new(router.clone())),
            router,
        }
    }

    fn begin_stop(&self) {
        self.endpoint.begin_stop();
    }

    async fn serve(self: Arc<Self>, home: &Path) -> Result<()> {
        self.endpoint.clone().serve(home).await
    }
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
    pub(crate) fn start(home: PathBuf, router: Arc<Router>) -> Result<Self> {
        let lock = acquire_daemon_lock(&home)?;
        Ok(Self {
            home,
            daemon: Arc::new(Daemon::new(router)),
            _lock: lock,
        })
    }

    pub(crate) fn stop_handle(&self) -> Stop {
        Stop {
            daemon: Arc::downgrade(&self.daemon),
        }
    }

    pub(crate) async fn run(self) -> Result<()> {
        let serve_result = self.daemon.clone().serve(&self.home).await;
        let shutdown_result = self
            .daemon
            .router
            .shutdown()
            .await
            .context("drain Station placements");
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
    selection: crate::config::Selection,
) -> Result<()> {
    let identity = selection.identity_dir()?;
    let config_root = crate::config::config_root()?;
    let self_contained = selection.self_contained();
    let agents_base = crate::registry::agents_base(&config_root);
    let home = selection.daemon_home()?;
    let router = Arc::new(Router::new(
        Catalog::new(identity, agents_base, self_contained),
        packages,
    ));
    let runner = Runner::start(home, router)?;
    let stop = runner.stop_handle();
    let signal = tokio::spawn(async move {
        shutdown_signal().await;
        stop.stop();
    });
    let result = runner.run().await;
    signal.abort();
    let _ = signal.await;
    result
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
    )
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
        let (mechanics, _) = crate::orbital::form_space(&home, seed, "Host Test").unwrap();
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
                projects: Vec::new(),
            }],
        );
        let resolved = directory.resolve(id.as_str()).unwrap();
        (base, daemon_home, directory, resolved)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_host_endpoint_places_routes_streams_and_drains_an_orbit() {
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
        while !matches!(client.probe().await, control::Probe::Healthy) {
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
            client.request(route, &Request::Status, None).await.unwrap(),
            Response::Status(_)
        ));
        let call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList).unwrap();
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
        while !matches!(client.probe().await, control::Probe::Healthy) {
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
