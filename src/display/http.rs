//! Receiver-facing HTTPS routes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::Router;
use display_protocol::auth::{
    sha256, AssetRange, RequestContext, RequestMethod, RequestRoute, AUTHORIZATION_SCHEME,
    HEADER_ASSET, HEADER_ASSIGNMENT, HEADER_BODY_SHA256, HEADER_CHALLENGE, HEADER_CURRENT_ITEM,
    HEADER_DEVICE, HEADER_ELAPSED_MS, HEADER_NEXT_CHALLENGE, HEADER_PROGRAM, HEADER_PROTOCOL_MAJOR,
    HEADER_RANGE_LENGTH, HEADER_RANGE_START, HEADER_REVISION, HEADER_ROUTE, HEADER_WAIT_MS,
};
use display_protocol::bounds::{
    MAX_CAPABILITY_BODY_BYTES, MAX_HEALTH_BODY_BYTES, MAX_HTTP_BODY_BYTES, MAX_LONG_POLL_WAIT_MS,
    MAX_PAIRING_BODY_BYTES,
};
use display_protocol::ids::{
    AuthenticationTag, Challenge, DisplayAssetId, DisplayAssignmentId, DisplayDeviceId,
    DisplayProgramId, DisplayProgramItemId, ProgramRevision, Sha256Digest,
};
use display_protocol::pairing::{
    PairingCompleteRequest, PairingStartRequest, PairingStatusRequest,
};
use display_protocol::program::{
    DisplayAssetMediaType, DisplayPlayback, DisplayScene, MediaProtocol, ProgramChange,
};
use display_protocol::receiver::{
    ApiRefusal, ApiRefusalCode, ChallengeRequest, LiveTicketRequest, LiveTicketResponse,
    ReceiverCapabilities, ReceiverHealth,
};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tower_http::cors::{Any, CorsLayer};

use super::{
    AuthorizationRefusal, DisplayCoordinator, DisplayPairingService, DisplayTlsIdentity,
    LiveMediaPacket, LiveTransport,
};

#[derive(Clone)]
pub struct DisplayHttpState {
    pub coordinator: Arc<DisplayCoordinator>,
    pub pairing: Arc<DisplayPairingService>,
}

pub fn display_http_router(state: DisplayHttpState) -> Router {
    let exposed = [HeaderName::from_static("x-astrolabe-next-challenge")];
    let allowed = [
        AUTHORIZATION,
        ACCEPT,
        CONTENT_TYPE,
        RANGE,
        header_name(HEADER_PROTOCOL_MAJOR),
        header_name(HEADER_ROUTE),
        header_name(HEADER_DEVICE),
        header_name(HEADER_ASSIGNMENT),
        header_name(HEADER_PROGRAM),
        header_name(HEADER_REVISION),
        header_name(HEADER_CURRENT_ITEM),
        header_name(HEADER_ELAPSED_MS),
        header_name(HEADER_WAIT_MS),
        header_name(HEADER_ASSET),
        header_name(HEADER_RANGE_START),
        header_name(HEADER_RANGE_LENGTH),
        header_name(HEADER_CHALLENGE),
        header_name(HEADER_BODY_SHA256),
    ];
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(allowed)
        .expose_headers(exposed);
    display_routes()
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(cors)
        .with_state(state)
}

/// The routing table alone, before any state is attached.
///
/// Split out so it can be built in a test. `Router::route` validates each
/// pattern by **panicking** on a bad one, and the panic lands at daemon
/// startup rather than at a request — which is how
/// `/renditions/{rendition}.m3u8` took the whole daemon down with nothing to
/// catch it, because no test had ever constructed this router. Building the
/// real table (rather than a copied list of paths) is what keeps that check
/// honest as routes are added.
fn display_routes() -> Router<DisplayHttpState> {
    Router::new()
        .route("/head/v1/instance", get(instance))
        .route(
            "/head/v1/pairings",
            post(pairing_start).layer(DefaultBodyLimit::max(MAX_PAIRING_BODY_BYTES)),
        )
        .route(
            "/head/v1/pairings/status",
            post(pairing_status).layer(DefaultBodyLimit::max(MAX_PAIRING_BODY_BYTES)),
        )
        .route(
            "/head/v1/pairings/complete",
            post(pairing_complete).layer(DefaultBodyLimit::max(MAX_PAIRING_BODY_BYTES)),
        )
        .route(
            "/head/v1/challenges",
            post(challenge).layer(DefaultBodyLimit::max(MAX_PAIRING_BODY_BYTES)),
        )
        .route(
            "/head/v1/capabilities",
            post(capabilities).layer(DefaultBodyLimit::max(MAX_CAPABILITY_BODY_BYTES)),
        )
        .route("/head/v1/program", get(program_snapshot))
        .route("/head/v1/program/changes", get(program_changes))
        .route("/head/v1/assets/{asset}", get(asset))
        .route(
            "/head/v1/live/tickets",
            post(live_ticket).layer(DefaultBodyLimit::max(MAX_PAIRING_BODY_BYTES)),
        )
        .route("/head/v1/live/{ticket}/socket", any(live_socket))
        .route("/head/v1/live/{ticket}/master.m3u8", get(hls_master))
        // The parameter captures the whole filename, extension included, because
        // matchit allows only one parameter per path segment and refuses
        // `{rendition}.m3u8` — by panicking at insert, which is to say at daemon
        // startup. The wire URLs are unchanged: `/renditions/hi.m3u8` still
        // matches, and the handler strips the suffix it requires.
        .route(
            "/head/v1/live/{ticket}/renditions/{rendition_file}",
            get(hls_media_playlist),
        )
        .route(
            "/head/v1/live/{ticket}/segments/{sequence_file}",
            get(hls_segment),
        )
        .route(
            "/head/v1/health",
            post(health).layer(DefaultBodyLimit::max(MAX_HEALTH_BODY_BYTES)),
        )
}

/// Serve the closed receiver router over the coordinator's pinned certificate.
/// Take the display port, or say which kind of failure it was.
///
/// Split out of [`serve_display_https`] because the two failures need different
/// answers from the daemon above. **Could not take the port** is a
/// machine-arrangement fact — another daemon on this machine already holds it,
/// since the port is fixed and `0.0.0.0`-bound — and it is knowable before
/// anything is serving. **Stopped after serving** is this daemon's own service
/// breaking, which is the case worth failing on.
///
/// Folding them was why a second identity's daemon could not start at all on a
/// machine that already had one: the loser of the port race refused to exist
/// rather than coming up without a coordinator. That is the same shape as reading
/// an unreachable carrier as an empty mailbox — a fact that could not be
/// established, rendered as a verdict.
pub async fn bind_display(identity: &DisplayTlsIdentity) -> Result<TcpListener> {
    TcpListener::bind(identity.bind())
        .await
        .with_context(|| format!("bind display HTTPS on {}", identity.bind()))
}

/// Whether this failure is "somebody else already holds the port".
///
/// Matched on the io kind rather than the message, so it does not depend on
/// wording. `AddrInUse` is the one that means *another daemon*; a permission or
/// address error is this machine being configured in a way nobody should paper
/// over, and it keeps its fatality.
pub fn is_port_taken(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| io.kind() == std::io::ErrorKind::AddrInUse)
}

pub async fn serve_display_https(
    state: DisplayHttpState,
    identity: Arc<DisplayTlsIdentity>,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    let listener = bind_display(&identity).await?;
    serve_display_on(listener, state, identity, stop).await
}

/// Serve on a listener somebody else already took.
///
/// Split out because a probe is not a reservation. The daemon used to `bind_display`
/// to decide whether it could host displays, **drop the listener**, and then let this
/// function bind again — so anything taking the port in between arrived as a bind
/// failure on the serving path, where the degradation ladder does not run, and the
/// whole daemon died on exactly the condition the ladder was written for.
///
/// The window is not theoretical: two daemons starting close together both probe
/// successfully, because the probes serialise, and then one of them loses the real
/// bind and dies instead of degrading.
pub async fn serve_display_on(
    listener: TcpListener,
    state: DisplayHttpState,
    identity: Arc<DisplayTlsIdentity>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let acceptor = TlsAcceptor::from(identity.server_config());
    let app = display_http_router(state);
    let mut connections = JoinSet::new();
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.context("accept display HTTPS connection")?;
                let acceptor = acceptor.clone();
                let service = app.clone();
                connections.spawn(async move {
                    let tls = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream))
                        .await
                        .context("display TLS handshake timed out")??;
                    http1::Builder::new()
                        .serve_connection(
                            TokioIo::new(tls),
                            TowerToHyperService::new(service),
                        )
                        .with_upgrades()
                        .await
                        .with_context(|| format!("serve display HTTPS connection from {peer}"))
                });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                if let Some(Ok(Err(error))) = completed {
                    tracing::debug!(%error, "display HTTPS connection ended");
                }
            }
        }
    }
    connections.shutdown().await;
    Ok(())
}

async fn instance(State(state): State<DisplayHttpState>) -> Response<Body> {
    json(StatusCode::OK, state.pairing.instance())
}

async fn pairing_start(State(state): State<DisplayHttpState>, body: Bytes) -> Response<Body> {
    let request = match decode::<PairingStartRequest>(&body) {
        Ok(request) => request,
        Err(_) => return public_refusal(StatusCode::BAD_REQUEST, ApiRefusalCode::InvalidRequest),
    };
    match state.pairing.start(request, now()) {
        Ok(response) => json(StatusCode::OK, &response),
        Err(_) => public_refusal(StatusCode::BAD_REQUEST, ApiRefusalCode::InvalidRequest),
    }
}

async fn pairing_status(State(state): State<DisplayHttpState>, body: Bytes) -> Response<Body> {
    let request = match decode::<PairingStatusRequest>(&body) {
        Ok(request) => request,
        Err(_) => return public_refusal(StatusCode::BAD_REQUEST, ApiRefusalCode::InvalidRequest),
    };
    match state.pairing.status(request, now()) {
        Ok(response) => json(StatusCode::OK, &response),
        Err(_) => public_refusal(
            StatusCode::UNAUTHORIZED,
            ApiRefusalCode::AuthenticationFailed,
        ),
    }
}

async fn pairing_complete(State(state): State<DisplayHttpState>, body: Bytes) -> Response<Body> {
    let request = match decode::<PairingCompleteRequest>(&body) {
        Ok(request) => request,
        Err(_) => return public_refusal(StatusCode::BAD_REQUEST, ApiRefusalCode::InvalidRequest),
    };
    match state.pairing.complete(request, now()) {
        Ok(response) => json(StatusCode::OK, &response),
        Err(_) => public_refusal(
            StatusCode::UNAUTHORIZED,
            ApiRefusalCode::AuthenticationFailed,
        ),
    }
}

async fn challenge(State(state): State<DisplayHttpState>, body: Bytes) -> Response<Body> {
    let request = match decode::<ChallengeRequest>(&body) {
        Ok(request) if request.protocol_major == display_protocol::PROTOCOL_MAJOR => request,
        _ => return public_refusal(StatusCode::BAD_REQUEST, ApiRefusalCode::InvalidRequest),
    };
    match state.pairing.challenge(&request.device, now()) {
        Ok(response) => json(StatusCode::OK, &response),
        Err(error) => authorization_refusal(error),
    }
}

async fn capabilities(
    State(state): State<DisplayHttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let parsed = match AuthorizedRequest::parse(
        &headers,
        &body,
        RequestMethod::Post,
        RequestRoute::Capabilities,
        None,
    ) {
        Ok(parsed) => parsed,
        Err(_) => return auth_refusal(ApiRefusalCode::AuthenticationFailed),
    };
    let authorized = match parsed.authorize(&state, now()) {
        Ok(authorized) => authorized,
        Err(error) => return authorization_refusal(error),
    };
    let capabilities = match decode::<ReceiverCapabilities>(&body) {
        Ok(capabilities) => capabilities,
        Err(_) => {
            return consumed_refusal(
                ApiRefusalCode::InvalidRequest,
                authorized.next_challenge,
                StatusCode::BAD_REQUEST,
            )
        }
    };
    match state
        .pairing
        .accept_capabilities(&authorized.record.device, capabilities)
    {
        Ok(()) => accepted(authorized.next_challenge),
        Err(_) => consumed_refusal(
            ApiRefusalCode::BoundExceeded,
            authorized.next_challenge,
            StatusCode::BAD_REQUEST,
        ),
    }
}

async fn program_snapshot(
    State(state): State<DisplayHttpState>,
    headers: HeaderMap,
) -> Response<Body> {
    let parsed = match AuthorizedRequest::parse(
        &headers,
        &[],
        RequestMethod::Get,
        RequestRoute::ProgramSnapshot,
        None,
    ) {
        Ok(parsed) => parsed,
        Err(_) => return auth_refusal(ApiRefusalCode::AuthenticationFailed),
    };
    let authorized = match parsed.authorize(&state, now()) {
        Ok(authorized) => authorized,
        Err(error) => return authorization_refusal(error),
    };
    if state
        .coordinator
        .active_assignment_for_device(&authorized.record.device, now())
        .ok()
        .flatten()
        .is_none()
    {
        return with_challenge(
            json(StatusCode::OK, &ProgramChange::Unassigned),
            authorized.next_challenge,
        );
    }
    match state
        .coordinator
        .compile_for_device(
            &authorized.record.device,
            &authorized.record.capabilities,
            now(),
        )
        .await
    {
        Ok(compiled) => with_challenge(
            json(
                StatusCode::OK,
                &ProgramChange::Snapshot {
                    program: compiled.program.clone(),
                },
            ),
            authorized.next_challenge,
        ),
        Err(_) => consumed_refusal(
            ApiRefusalCode::TemporarilyUnavailable,
            authorized.next_challenge,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    }
}

async fn program_changes(
    State(state): State<DisplayHttpState>,
    headers: HeaderMap,
) -> Response<Body> {
    let parsed = match AuthorizedRequest::parse(
        &headers,
        &[],
        RequestMethod::Get,
        RequestRoute::ProgramChanges,
        None,
    ) {
        Ok(parsed)
            if parsed
                .wait_ms
                .is_some_and(|wait| wait <= MAX_LONG_POLL_WAIT_MS) =>
        {
            parsed
        }
        _ => return auth_refusal(ApiRefusalCode::AuthenticationFailed),
    };
    let authorized = match parsed.authorize(&state, now()) {
        Ok(authorized) => authorized,
        Err(error) => return authorization_refusal(error),
    };
    // Arm both controller and World doorbells before reading the assignment or
    // compiling. A mutation in the gap is then either reflected in that read
    // or waiting in one of these receivers; it cannot become a lost wakeup.
    let changes = state.coordinator.subscribe_changes();
    let Some(assignment) = state
        .coordinator
        .active_assignment_for_device(&authorized.record.device, now())
        .ok()
        .flatten()
    else {
        return with_challenge(
            json(StatusCode::OK, &ProgramChange::Unassigned),
            authorized.next_challenge,
        );
    };
    let first = state
        .coordinator
        .compile_for_device(
            &authorized.record.device,
            &authorized.record.capabilities,
            now(),
        )
        .await;
    let Ok(mut compiled) = first else {
        return consumed_refusal(
            ApiRefusalCode::TemporarilyUnavailable,
            authorized.next_challenge,
            StatusCode::SERVICE_UNAVAILABLE,
        );
    };
    if parsed.revision.as_ref() == Some(&compiled.program.revision) {
        let requested_wait = parsed.wait_ms.unwrap_or(1);
        let scheduled_wait = compiled
            .refresh_after_ms
            .map_or(requested_wait, |refresh| refresh.min(requested_wait));
        state
            .coordinator
            .wait_for_change(
                &assignment,
                changes,
                Duration::from_millis(u64::from(scheduled_wait)),
            )
            .await;
        if state
            .coordinator
            .active_assignment_for_device(&authorized.record.device, now())
            .ok()
            .flatten()
            .is_none()
        {
            return with_challenge(
                json(StatusCode::OK, &ProgramChange::Unassigned),
                authorized.next_challenge,
            );
        }
        match state
            .coordinator
            .compile_for_device(
                &authorized.record.device,
                &authorized.record.capabilities,
                now(),
            )
            .await
        {
            Ok(refreshed) => compiled = refreshed,
            Err(_) => {
                return consumed_refusal(
                    ApiRefusalCode::TemporarilyUnavailable,
                    authorized.next_challenge,
                    StatusCode::SERVICE_UNAVAILABLE,
                )
            }
        }
    }
    let change = if parsed.revision.as_ref() != Some(&compiled.program.revision) {
        ProgramChange::Snapshot {
            program: compiled.program.clone(),
        }
    } else {
        let Some(current_item) = parsed.current_item.as_ref() else {
            return consumed_refusal(
                ApiRefusalCode::InvalidRequest,
                authorized.next_challenge,
                StatusCode::BAD_REQUEST,
            );
        };
        let Some(index) = compiled
            .program
            .items
            .iter()
            .position(|item| &item.id == current_item)
            .and_then(|index| u16::try_from(index).ok())
        else {
            return with_challenge(
                json(
                    StatusCode::OK,
                    &ProgramChange::Reset {
                        reason: display_protocol::program::ResetReason::CursorCorrection,
                    },
                ),
                authorized.next_challenge,
            );
        };
        let playback = if compiled.program.playback.sync.is_some() {
            compiled.program.playback.clone()
        } else {
            DisplayPlayback {
                current_index: index,
                elapsed_ms: parsed.elapsed_ms.unwrap_or(0),
                cycle: compiled.program.playback.cycle,
                sync: None,
            }
        };
        ProgramChange::NoChange {
            revision: compiled.program.revision.clone(),
            playback,
        }
    };
    with_challenge(json(StatusCode::OK, &change), authorized.next_challenge)
}

async fn asset(
    State(state): State<DisplayHttpState>,
    Path(asset_path): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let asset_id = match DisplayAssetId::parse(asset_path) {
        Ok(asset) => asset,
        Err(_) => return auth_refusal(ApiRefusalCode::InvalidRequest),
    };
    let parsed = match AuthorizedRequest::parse(
        &headers,
        &[],
        RequestMethod::Get,
        RequestRoute::Asset,
        Some(&asset_id),
    ) {
        Ok(parsed) => parsed,
        Err(_) => return auth_refusal(ApiRefusalCode::AuthenticationFailed),
    };
    let authorized = match parsed.authorize(&state, now()) {
        Ok(authorized) => authorized,
        Err(error) => return authorization_refusal(error),
    };
    let compiled = match state
        .coordinator
        .compile_for_device(
            &authorized.record.device,
            &authorized.record.capabilities,
            now(),
        )
        .await
    {
        Ok(compiled) => compiled,
        Err(_) => {
            return consumed_refusal(
                ApiRefusalCode::TemporarilyUnavailable,
                authorized.next_challenge,
                StatusCode::SERVICE_UNAVAILABLE,
            )
        }
    };
    if parsed.assignment.as_ref() != Some(&compiled.program.assignment)
        || parsed.program.as_ref() != Some(&compiled.program.program)
        || parsed.revision.as_ref() != Some(&compiled.program.revision)
    {
        return consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::CONFLICT,
        );
    }
    let Some(bytes) = compiled.asset(&asset_id) else {
        return consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::NOT_FOUND,
        );
    };
    let media_type = compiled
        .program
        .items
        .iter()
        .find_map(|item| match &item.scene {
            DisplayScene::Frame { asset }
            | DisplayScene::Media {
                manifest: asset, ..
            } if asset.id == asset_id => Some(asset.media_type),
            _ => None,
        });
    let Some(media_type) = media_type else {
        return consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::NOT_FOUND,
        );
    };
    let (status, body, content_range) = match parsed.range {
        Some(range) => match ranged(bytes, range) {
            Ok((slice, content_range)) => (StatusCode::PARTIAL_CONTENT, slice, Some(content_range)),
            Err(_) => {
                return consumed_refusal(
                    ApiRefusalCode::InvalidRequest,
                    authorized.next_challenge,
                    StatusCode::RANGE_NOT_SATISFIABLE,
                )
            }
        },
        None => (StatusCode::OK, bytes.to_vec(), None),
    };
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    if let Ok(value) = HeaderValue::from_str(media_content_type(media_type)) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    if let Some(content_range) = content_range {
        if let Ok(value) = HeaderValue::from_str(&content_range) {
            response.headers_mut().insert(CONTENT_RANGE, value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, value);
    }
    with_challenge(response, authorized.next_challenge)
}

async fn health(
    State(state): State<DisplayHttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let parsed = match AuthorizedRequest::parse(
        &headers,
        &body,
        RequestMethod::Post,
        RequestRoute::Health,
        None,
    ) {
        Ok(parsed) => parsed,
        Err(_) => return auth_refusal(ApiRefusalCode::AuthenticationFailed),
    };
    let authorized = match parsed.authorize(&state, now()) {
        Ok(authorized) => authorized,
        Err(error) => return authorization_refusal(error),
    };
    let health = match decode::<ReceiverHealth>(&body) {
        Ok(health) => health,
        Err(_) => {
            return consumed_refusal(
                ApiRefusalCode::InvalidRequest,
                authorized.next_challenge,
                StatusCode::BAD_REQUEST,
            )
        }
    };
    if parsed.revision.as_ref() != Some(&health.revision)
        || parsed.current_item.as_ref() != Some(&health.current_item)
        || parsed.elapsed_ms != Some(health.elapsed_ms)
    {
        return consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::BAD_REQUEST,
        );
    }
    let current = match state.coordinator.current_program(&authorized.record.device) {
        Ok(Some(program)) => program,
        Ok(None) => {
            return consumed_refusal(
                ApiRefusalCode::TemporarilyUnavailable,
                authorized.next_challenge,
                StatusCode::SERVICE_UNAVAILABLE,
            )
        }
        Err(_) => {
            return consumed_refusal(
                ApiRefusalCode::TemporarilyUnavailable,
                authorized.next_challenge,
                StatusCode::SERVICE_UNAVAILABLE,
            )
        }
    };
    let item_belongs = current
        .items
        .iter()
        .any(|item| item.id == health.current_item);
    let displayed_asset_belongs = health
        .last_displayed_asset
        .as_ref()
        .is_none_or(|displayed| {
            current.items.iter().any(|item| {
                matches!(
                    &item.scene,
                    display_protocol::program::DisplayScene::Frame { asset }
                        if asset.id == displayed.id && asset.sha256 == displayed.sha256
                )
            })
        });
    if parsed.assignment.as_ref() != Some(&current.assignment)
        || parsed.program.as_ref() != Some(&current.program)
        || health.revision != current.revision
        || !item_belongs
        || !displayed_asset_belongs
    {
        return consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::CONFLICT,
        );
    }
    match state
        .pairing
        .record_health(&authorized.record.device, health)
    {
        Ok(()) => accepted(authorized.next_challenge),
        Err(_) => consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::BAD_REQUEST,
        ),
    }
}

async fn live_ticket(
    State(state): State<DisplayHttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let request = match decode::<LiveTicketRequest>(&body) {
        Ok(request) => request,
        Err(_) => return auth_refusal(ApiRefusalCode::InvalidRequest),
    };
    let manifest = match one_header(&headers, HEADER_ASSET)
        .and_then(|value| value.ok_or_else(|| anyhow!("missing live manifest")))
        .and_then(|value| DisplayAssetId::parse(value.to_string()).map_err(Into::into))
    {
        Ok(manifest) => manifest,
        Err(_) => return auth_refusal(ApiRefusalCode::AuthenticationFailed),
    };
    let parsed = match AuthorizedRequest::parse(
        &headers,
        &body,
        RequestMethod::Post,
        RequestRoute::LiveTicket,
        Some(&manifest),
    ) {
        Ok(parsed) => parsed,
        Err(_) => return auth_refusal(ApiRefusalCode::AuthenticationFailed),
    };
    let authorized = match parsed.authorize(&state, now()) {
        Ok(authorized) => authorized,
        Err(error) => return authorization_refusal(error),
    };
    let compiled = match state
        .coordinator
        .compile_for_device(
            &authorized.record.device,
            &authorized.record.capabilities,
            now(),
        )
        .await
    {
        Ok(compiled) => compiled,
        Err(_) => {
            return consumed_refusal(
                ApiRefusalCode::TemporarilyUnavailable,
                authorized.next_challenge,
                StatusCode::SERVICE_UNAVAILABLE,
            )
        }
    };
    let Some(current_item) = parsed.current_item.as_ref() else {
        return consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::BAD_REQUEST,
        );
    };
    if parsed.assignment.as_ref() != Some(&compiled.program.assignment)
        || parsed.program.as_ref() != Some(&compiled.program.program)
        || parsed.revision.as_ref() != Some(&compiled.program.revision)
    {
        return consumed_refusal(
            ApiRefusalCode::InvalidRequest,
            authorized.next_challenge,
            StatusCode::CONFLICT,
        );
    }
    let transport = match request.transport {
        MediaProtocol::Mse => LiveTransport::Mse,
        MediaProtocol::Hls => LiveTransport::Hls,
        MediaProtocol::Dash => {
            return consumed_refusal(
                ApiRefusalCode::InvalidRequest,
                authorized.next_challenge,
                StatusCode::BAD_REQUEST,
            )
        }
    };
    let grant = match state
        .coordinator
        .mint_live_ticket(
            &authorized.record.device,
            current_item,
            &manifest,
            transport,
            now(),
        )
        .await
    {
        Ok(grant) => grant,
        Err(_) => {
            return consumed_refusal(
                ApiRefusalCode::TemporarilyUnavailable,
                authorized.next_challenge,
                StatusCode::SERVICE_UNAVAILABLE,
            )
        }
    };
    let suffix = match transport {
        LiveTransport::Mse => "socket",
        LiveTransport::Hls => "master.m3u8",
    };
    with_challenge(
        json(
            StatusCode::OK,
            &LiveTicketResponse {
                protocol_major: display_protocol::PROTOCOL_MAJOR,
                transport: request.transport,
                endpoint: format!("/head/v1/live/{}/{suffix}", grant.token),
                expires_at_unix_ms: grant.expires_at_unix_ms,
            },
        ),
        authorized.next_challenge,
    )
}

async fn live_socket(
    State(state): State<DisplayHttpState>,
    Path(ticket): Path<String>,
    ws: WebSocketUpgrade,
) -> Response<Body> {
    if !valid_ticket_token(&ticket) {
        return public_refusal(StatusCode::NOT_FOUND, ApiRefusalCode::InvalidRequest);
    }
    let stream =
        match state
            .coordinator
            .authorize_live_ticket(&ticket, LiveTransport::Mse, true, now())
        {
            Ok(stream) => stream,
            Err(_) => return public_refusal(StatusCode::FORBIDDEN, ApiRefusalCode::Revoked),
        };
    let snapshot = match state
        .coordinator
        .live_hub()
        .mse_snapshot(&stream.orbit, &stream.resource)
    {
        Ok(snapshot) => snapshot,
        Err(_) => {
            return public_refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                ApiRefusalCode::TemporarilyUnavailable,
            )
        }
    };
    ws.on_upgrade(move |socket| serve_live_socket(socket, state, stream, snapshot))
        .into_response()
}

async fn serve_live_socket(
    mut socket: WebSocket,
    state: DisplayHttpState,
    stream: super::coordinator::AuthorizedLiveStream,
    mut snapshot: super::LiveMediaSnapshot,
) {
    let hello = serde_json::json!({
        "kind": "astrolabe_live",
        "version": 1,
        "tracks": snapshot.tracks,
    });
    let Ok(hello) = serde_json::to_string(&hello) else {
        return;
    };
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    let renditions = snapshot
        .tracks
        .iter()
        .map(|track| track.rendition.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for packet in snapshot.packets {
        if send_live_packet(&mut socket, packet).await.is_err() {
            return;
        }
    }
    let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
    authorization_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = authorization_check.tick() => {
                if !state.coordinator.live_stream_still_authorized(&stream, now()) {
                    return;
                }
            }
            update = snapshot.updates.recv() => {
                let Ok(packet) = update else { return };
                let rendition = match &packet {
                    LiveMediaPacket::Init { rendition, .. }
                    | LiveMediaPacket::Fragment { rendition, .. } => rendition,
                };
                if renditions.contains(rendition)
                    && send_live_packet(&mut socket, packet).await.is_err()
                {
                    return;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    _ => {}
                }
            }
        }
    }
}

async fn send_live_packet(socket: &mut WebSocket, packet: LiveMediaPacket) -> Result<()> {
    let (kind, rendition, sequence, published, start, duration, discontinuity, payload) =
        match packet {
            LiveMediaPacket::Init { rendition, bytes } => {
                (1u8, rendition, 0, 0, 0, 0, false, bytes)
            }
            LiveMediaPacket::Fragment {
                rendition,
                group_sequence,
                published_at_micros,
                start_timestamp,
                duration,
                discontinuity,
                bytes,
            } => (
                2,
                rendition,
                group_sequence,
                published_at_micros,
                start_timestamp,
                duration,
                discontinuity,
                bytes,
            ),
        };
    let name_length = u16::try_from(rendition.len()).context("live rendition name")?;
    let capacity = 36usize
        .checked_add(rendition.len())
        .and_then(|value| value.checked_add(payload.len()))
        .context("live WebSocket message size")?;
    let mut wire = Vec::with_capacity(capacity);
    wire.push(kind);
    wire.extend_from_slice(&name_length.to_be_bytes());
    wire.extend_from_slice(rendition.as_bytes());
    wire.extend_from_slice(&sequence.to_be_bytes());
    wire.extend_from_slice(&published.to_be_bytes());
    wire.extend_from_slice(&start.to_be_bytes());
    wire.extend_from_slice(&duration.to_be_bytes());
    wire.push(u8::from(discontinuity));
    wire.extend_from_slice(&payload);
    socket
        .send(Message::Binary(wire.into()))
        .await
        .context("send live WebSocket packet")
}

async fn hls_master(
    State(state): State<DisplayHttpState>,
    Path(ticket): Path<String>,
) -> Response<Body> {
    let Some(stream) = hls_authorization(&state, &ticket) else {
        return public_refusal(StatusCode::FORBIDDEN, ApiRefusalCode::Revoked);
    };
    match state
        .coordinator
        .live_hub()
        .hls_master(&stream.orbit, &stream.resource, ".")
    {
        Ok(playlist) => media_response("application/vnd.apple.mpegurl", playlist.into_bytes()),
        Err(_) => public_refusal(StatusCode::NOT_FOUND, ApiRefusalCode::InvalidRequest),
    }
}

/// The rendition name inside a media-playlist filename.
///
/// The extension is required here rather than by the router, because matchit
/// allows only one parameter per path segment. Wire URLs are unchanged; what
/// moved is where the `.m3u8` is checked. A filename without it is `None`, so
/// it becomes the same 404 an unknown rendition gets.
fn rendition_name(file: &str) -> Option<&str> {
    file.strip_suffix(".m3u8").filter(|name| !name.is_empty())
}

/// The sequence number inside a segment filename. Same reasoning; a malformed
/// number is a 404 rather than an extractor rejection, so the two unreachable
/// cases answer alike.
fn segment_sequence(file: &str) -> Option<u64> {
    file.strip_suffix(".ts")?.parse().ok()
}

async fn hls_media_playlist(
    State(state): State<DisplayHttpState>,
    Path((ticket, rendition_file)): Path<(String, String)>,
) -> Response<Body> {
    let Some(rendition) = rendition_name(&rendition_file) else {
        return public_refusal(StatusCode::NOT_FOUND, ApiRefusalCode::InvalidRequest);
    };
    let Some(stream) = hls_authorization(&state, &ticket) else {
        return public_refusal(StatusCode::FORBIDDEN, ApiRefusalCode::Revoked);
    };
    match state.coordinator.live_hub().hls_media_playlist(
        &stream.orbit,
        &stream.resource,
        rendition,
        "..",
    ) {
        Ok(playlist) => media_response("application/vnd.apple.mpegurl", playlist.into_bytes()),
        Err(_) => public_refusal(StatusCode::NOT_FOUND, ApiRefusalCode::InvalidRequest),
    }
}

async fn hls_segment(
    State(state): State<DisplayHttpState>,
    Path((ticket, sequence_file)): Path<(String, String)>,
) -> Response<Body> {
    let Some(sequence) = segment_sequence(&sequence_file) else {
        return public_refusal(StatusCode::NOT_FOUND, ApiRefusalCode::InvalidRequest);
    };
    let Some(stream) = hls_authorization(&state, &ticket) else {
        return public_refusal(StatusCode::FORBIDDEN, ApiRefusalCode::Revoked);
    };
    // The coordinator answers from wherever this presentation keeps its
    // segments: a live window holds them materialised, a planned one builds
    // the asked-for segment from the content plane and nothing else.
    match state
        .coordinator
        .hls_segment(&stream, sequence, now())
        .await
    {
        Ok(segment) => media_response("video/mp2t", segment),
        Err(_) => public_refusal(StatusCode::NOT_FOUND, ApiRefusalCode::InvalidRequest),
    }
}

fn hls_authorization(
    state: &DisplayHttpState,
    ticket: &str,
) -> Option<super::coordinator::AuthorizedLiveStream> {
    valid_ticket_token(ticket)
        .then(|| {
            state
                .coordinator
                .authorize_live_ticket(ticket, LiveTransport::Hls, false, now())
        })
        .and_then(Result::ok)
}

fn media_response(content_type: &'static str, bytes: Vec<u8>) -> Response<Body> {
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn valid_ticket_token(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct AuthorizedRequest {
    method: RequestMethod,
    route: RequestRoute,
    device: DisplayDeviceId,
    assignment: Option<DisplayAssignmentId>,
    program: Option<DisplayProgramId>,
    revision: Option<ProgramRevision>,
    current_item: Option<DisplayProgramItemId>,
    elapsed_ms: Option<u32>,
    wait_ms: Option<u32>,
    asset: Option<DisplayAssetId>,
    range: Option<AssetRange>,
    challenge: Challenge,
    body_sha256: Sha256Digest,
    tag: AuthenticationTag,
}

impl AuthorizedRequest {
    fn parse(
        headers: &HeaderMap,
        body: &[u8],
        method: RequestMethod,
        route: RequestRoute,
        path_asset: Option<&DisplayAssetId>,
    ) -> Result<Self> {
        let authorization = one_header(headers, AUTHORIZATION.as_str())?
            .ok_or_else(|| anyhow!("missing display authorization"))?;
        let tag = authorization
            .strip_prefix(&format!("{AUTHORIZATION_SCHEME} "))
            .ok_or_else(|| anyhow!("invalid display authorization scheme"))?;
        let tag = AuthenticationTag::parse(tag.to_string())?;
        let protocol = parse_required::<u32>(headers, HEADER_PROTOCOL_MAJOR)?;
        if protocol != display_protocol::PROTOCOL_MAJOR {
            return Err(anyhow!("unsupported display protocol"));
        }
        if one_header(headers, HEADER_ROUTE)? != Some(route.wire_name()) {
            return Err(anyhow!("display route header mismatch"));
        }
        let device = DisplayDeviceId::parse(required(headers, HEADER_DEVICE)?.to_string())?;
        let assignment = parse_optional(headers, HEADER_ASSIGNMENT, DisplayAssignmentId::parse)?;
        let program = parse_optional(headers, HEADER_PROGRAM, DisplayProgramId::parse)?;
        let revision = parse_optional(headers, HEADER_REVISION, ProgramRevision::parse)?;
        let current_item =
            parse_optional(headers, HEADER_CURRENT_ITEM, DisplayProgramItemId::parse)?;
        let elapsed_ms = parse_optional_number(headers, HEADER_ELAPSED_MS)?;
        let wait_ms = parse_optional_number(headers, HEADER_WAIT_MS)?;
        let asset = parse_optional(headers, HEADER_ASSET, DisplayAssetId::parse)?;
        if path_asset.is_some() && asset.as_ref() != path_asset {
            return Err(anyhow!("display asset path/header mismatch"));
        }
        let range_start = parse_optional_number::<u64>(headers, HEADER_RANGE_START)?;
        let range_length = parse_optional_number::<u32>(headers, HEADER_RANGE_LENGTH)?;
        let range = match (range_start, range_length) {
            (Some(start), Some(length)) if length > 0 => Some(AssetRange { start, length }),
            (None, None) => None,
            _ => return Err(anyhow!("invalid display asset range")),
        };
        validate_range_header(headers, range)?;
        let challenge = Challenge::parse(required(headers, HEADER_CHALLENGE)?.to_string())?;
        let body_sha256 = Sha256Digest::parse(required(headers, HEADER_BODY_SHA256)?.to_string())?;
        if sha256(body)? != body_sha256 {
            return Err(anyhow!("display body digest mismatch"));
        }
        let parsed = Self {
            method,
            route,
            device,
            assignment,
            program,
            revision,
            current_item,
            elapsed_ms,
            wait_ms,
            asset,
            range,
            challenge,
            body_sha256,
            tag,
        };
        // Build once here to enforce the route's closed optional-field shape
        // before enrollment lookup or challenge consumption.
        display_protocol::auth::request_transcript(&parsed.context())?;
        Ok(parsed)
    }

    fn context(&self) -> RequestContext<'_> {
        RequestContext {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            method: self.method,
            route: self.route,
            device: &self.device,
            assignment: self.assignment.as_ref(),
            program: self.program.as_ref(),
            revision: self.revision.as_ref(),
            current_item: self.current_item.as_ref(),
            elapsed_ms: self.elapsed_ms,
            wait_ms: self.wait_ms,
            asset: self.asset.as_ref(),
            range: self.range,
            challenge: &self.challenge,
            body_sha256: &self.body_sha256,
        }
    }

    fn authorize(
        &self,
        state: &DisplayHttpState,
        now_unix_ms: u64,
    ) -> std::result::Result<super::AuthorizedDevice, AuthorizationRefusal> {
        state
            .pairing
            .authorize(&self.context(), &self.tag, now_unix_ms)
    }
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).context("decode display JSON")
}

fn now() -> u64 {
    mechanics::wallclock::now_millis()
}

fn accepted(challenge: Challenge) -> Response<Body> {
    with_challenge(
        json(StatusCode::OK, &serde_json::json!({"kind": "accepted"})),
        challenge,
    )
}

fn public_refusal(status: StatusCode, code: ApiRefusalCode) -> Response<Body> {
    json(
        status,
        &ApiRefusal {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            code,
            retry_after_ms: None,
            next_challenge: None,
        },
    )
}

fn auth_refusal(code: ApiRefusalCode) -> Response<Body> {
    public_refusal(StatusCode::UNAUTHORIZED, code)
}

fn authorization_refusal(error: AuthorizationRefusal) -> Response<Body> {
    match error {
        AuthorizationRefusal::NotEnrolled => auth_refusal(ApiRefusalCode::NotEnrolled),
        AuthorizationRefusal::Revoked => {
            public_refusal(StatusCode::FORBIDDEN, ApiRefusalCode::Revoked)
        }
        AuthorizationRefusal::ChallengeUnavailable => {
            public_refusal(StatusCode::CONFLICT, ApiRefusalCode::ChallengeConsumed)
        }
        AuthorizationRefusal::ChallengeExpired => auth_refusal(ApiRefusalCode::ChallengeExpired),
        AuthorizationRefusal::ChallengeConsumed => {
            public_refusal(StatusCode::CONFLICT, ApiRefusalCode::ChallengeConsumed)
        }
        AuthorizationRefusal::Authentication => auth_refusal(ApiRefusalCode::AuthenticationFailed),
        AuthorizationRefusal::Internal(error) => {
            tracing::error!(%error, "display authorization failed internally");
            public_refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                ApiRefusalCode::TemporarilyUnavailable,
            )
        }
    }
}

fn consumed_refusal(
    code: ApiRefusalCode,
    challenge: Challenge,
    status: StatusCode,
) -> Response<Body> {
    with_challenge(
        json(
            status,
            &ApiRefusal {
                protocol_major: display_protocol::PROTOCOL_MAJOR,
                code,
                retry_after_ms: None,
                next_challenge: Some(challenge.clone()),
            },
        ),
        challenge,
    )
}

fn with_challenge(mut response: Response<Body>, challenge: Challenge) -> Response<Body> {
    if let Ok(value) = HeaderValue::from_str(challenge.as_str()) {
        response
            .headers_mut()
            .insert(header_name(HEADER_NEXT_CHALLENGE), value);
    }
    response
}

fn json<T: Serialize>(status: StatusCode, value: &T) -> Response<Body> {
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            *response.status_mut() = status;
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            response
        }
        Err(_) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty())),
    }
}

fn header_name(name: &'static str) -> HeaderName {
    HeaderName::from_static(match name {
        HEADER_PROTOCOL_MAJOR => "x-astrolabe-protocol-major",
        HEADER_ROUTE => "x-astrolabe-route",
        HEADER_DEVICE => "x-astrolabe-device",
        HEADER_ASSIGNMENT => "x-astrolabe-assignment",
        HEADER_PROGRAM => "x-astrolabe-program",
        HEADER_REVISION => "x-astrolabe-revision",
        HEADER_CURRENT_ITEM => "x-astrolabe-current-item",
        HEADER_ELAPSED_MS => "x-astrolabe-elapsed-ms",
        HEADER_WAIT_MS => "x-astrolabe-wait-ms",
        HEADER_ASSET => "x-astrolabe-asset",
        HEADER_RANGE_START => "x-astrolabe-range-start",
        HEADER_RANGE_LENGTH => "x-astrolabe-range-length",
        HEADER_CHALLENGE => "x-astrolabe-challenge",
        HEADER_BODY_SHA256 => "x-astrolabe-body-sha256",
        HEADER_NEXT_CHALLENGE => "x-astrolabe-next-challenge",
        _ => "x-astrolabe-invalid",
    })
}

fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(anyhow!("duplicate display protocol header"));
    }
    first
        .map(|value| value.to_str().context("display header is not ASCII"))
        .transpose()
}

fn required<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    one_header(headers, name)?.ok_or_else(|| anyhow!("missing display protocol header"))
}

fn parse_required<T: std::str::FromStr>(headers: &HeaderMap, name: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    required(headers, name)?
        .parse::<T>()
        .map_err(|error| anyhow!("invalid display header: {error}"))
}

fn parse_optional<T, E>(
    headers: &HeaderMap,
    name: &str,
    parse: impl FnOnce(String) -> std::result::Result<T, E>,
) -> Result<Option<T>>
where
    E: std::fmt::Display,
{
    one_header(headers, name)?
        .map(|value| parse(value.to_string()).map_err(|error| anyhow!(error.to_string())))
        .transpose()
}

fn parse_optional_number<T>(headers: &HeaderMap, name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    one_header(headers, name)?
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|error| anyhow!("invalid display numeric header: {error}"))
        })
        .transpose()
}

fn validate_range_header(headers: &HeaderMap, range: Option<AssetRange>) -> Result<()> {
    let wire = one_header(headers, RANGE.as_str())?;
    match (range, wire) {
        (None, None) => Ok(()),
        (Some(range), Some(wire)) => {
            let end = range
                .start
                .checked_add(u64::from(range.length))
                .and_then(|exclusive| exclusive.checked_sub(1))
                .ok_or_else(|| anyhow!("display range overflow"))?;
            if wire == format!("bytes={}-{}", range.start, end) {
                Ok(())
            } else {
                Err(anyhow!("display Range header mismatch"))
            }
        }
        _ => Err(anyhow!("display range fields are incomplete")),
    }
}

fn ranged(bytes: &[u8], range: AssetRange) -> Result<(Vec<u8>, String)> {
    let start = usize::try_from(range.start).context("display asset range start")?;
    let end = start
        .checked_add(usize::try_from(range.length).context("display asset range length")?)
        .ok_or_else(|| anyhow!("display asset range overflow"))?;
    let slice = bytes
        .get(start..end)
        .ok_or_else(|| anyhow!("display asset range is out of bounds"))?;
    let inclusive = end.checked_sub(1).ok_or_else(|| anyhow!("empty range"))?;
    Ok((
        slice.to_vec(),
        format!("bytes {start}-{inclusive}/{}", bytes.len()),
    ))
}

fn media_content_type(media_type: DisplayAssetMediaType) -> &'static str {
    match media_type {
        DisplayAssetMediaType::ImagePng => "image/png",
        DisplayAssetMediaType::ImageJpeg => "image/jpeg",
        DisplayAssetMediaType::ImageWebp => "image/webp",
        DisplayAssetMediaType::MseManifest => "application/vnd.astrolabe.live+json",
        DisplayAssetMediaType::HlsManifest => "application/vnd.apple.mpegurl",
        DisplayAssetMediaType::DashManifest => "application/dash+xml",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_display_route_is_a_pattern_the_router_accepts() {
        // `Router::route` validates a pattern by PANICKING on a bad one, and
        // the panic lands at daemon startup rather than at a request. That is
        // how `/renditions/{rendition}.m3u8` shipped: matchit forbids a
        // parameter sharing a segment with a literal, `lait daemon` aborted
        // before printing anything, and no test had ever built this router.
        //
        // Building the real table — not a copied list of paths — is what keeps
        // this honest as routes are added.
        let _ = display_routes();
    }

    #[test]
    fn the_hls_filenames_keep_their_extensions_and_reject_what_lacks_them() {
        assert_eq!(rendition_name("hi.m3u8"), Some("hi"));
        assert_eq!(rendition_name("720p.m3u8"), Some("720p"));
        assert_eq!(
            rendition_name("hi"),
            None,
            "a request without the extension is not a rendition"
        );
        assert_eq!(
            rendition_name(".m3u8"),
            None,
            "and an empty name is not one either"
        );
        assert_eq!(rendition_name("hi.ts"), None, "nor is the wrong extension");

        assert_eq!(segment_sequence("42.ts"), Some(42));
        assert_eq!(segment_sequence("0.ts"), Some(0));
        assert_eq!(segment_sequence("42"), None);
        assert_eq!(segment_sequence("42.m3u8"), None);
        assert_eq!(
            segment_sequence("-1.ts"),
            None,
            "a malformed sequence answers as the unknown one does, not as a rejection"
        );
    }
}
