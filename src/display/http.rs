//! Receiver-facing HTTPS routes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode};
use axum::routing::{get, post};
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
    DisplayAssetMediaType, DisplayPlayback, DisplayScene, ProgramChange,
};
use display_protocol::receiver::{
    ApiRefusal, ApiRefusalCode, ChallengeRequest, ReceiverCapabilities, ReceiverHealth,
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
    DisplayAuthorizationError, DisplayCoordinator, DisplayPairingService, DisplayTlsIdentity,
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
            "/head/v1/health",
            post(health).layer(DefaultBodyLimit::max(MAX_HEALTH_BODY_BYTES)),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(cors)
        .with_state(state)
}

/// Serve the closed receiver router over the coordinator's pinned certificate.
pub async fn serve_display_https(
    state: DisplayHttpState,
    identity: Arc<DisplayTlsIdentity>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let listener = TcpListener::bind(identity.bind())
        .await
        .with_context(|| format!("bind display HTTPS on {}", identity.bind()))?;
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
    ) -> std::result::Result<super::AuthorizedDevice, DisplayAuthorizationError> {
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

fn authorization_refusal(error: DisplayAuthorizationError) -> Response<Body> {
    match error {
        DisplayAuthorizationError::NotEnrolled => auth_refusal(ApiRefusalCode::NotEnrolled),
        DisplayAuthorizationError::Revoked => {
            public_refusal(StatusCode::FORBIDDEN, ApiRefusalCode::Revoked)
        }
        DisplayAuthorizationError::ChallengeUnavailable => {
            public_refusal(StatusCode::CONFLICT, ApiRefusalCode::ChallengeConsumed)
        }
        DisplayAuthorizationError::ChallengeExpired => {
            auth_refusal(ApiRefusalCode::ChallengeExpired)
        }
        DisplayAuthorizationError::ChallengeConsumed => {
            public_refusal(StatusCode::CONFLICT, ApiRefusalCode::ChallengeConsumed)
        }
        DisplayAuthorizationError::Authentication => {
            auth_refusal(ApiRefusalCode::AuthenticationFailed)
        }
        DisplayAuthorizationError::Internal(error) => {
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
        DisplayAssetMediaType::HlsManifest => "application/vnd.apple.mpegurl",
        DisplayAssetMediaType::DashManifest => "application/dash+xml",
    }
}
