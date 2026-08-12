use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lait::serve::auth::{cookie_value, Guard, Refusal};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::contract::{
    ApiError, BackendEvent, CreateDeviceRequest, DeviceAction, EventKind, HistoryQuery, LogQuery,
};
use crate::{Supervisor, SupervisorError};

struct AppState {
    supervisor: Supervisor,
    guard: Guard,
    cookie_name: String,
}

pub fn router(supervisor: Supervisor, token: String, port: u16) -> Router {
    let state = Arc::new(AppState {
        supervisor,
        guard: Guard::new(token, port),
        cookie_name: format!("lait_workbench_token_{port}"),
    });
    Router::new()
        .route("/api/workbench/session", post(open_session))
        .route("/api/workbench/contract", get(contract))
        .route("/api/workbench/snapshot", get(snapshot))
        .route("/api/workbench/events", get(events))
        .route("/api/workbench/history/events", get(event_history))
        .route(
            "/api/workbench/history/connections",
            get(connection_history),
        )
        .route("/api/workbench/devices", post(create_device))
        .route("/api/workbench/devices/{id}/logs", get(device_logs))
        .route("/api/workbench/devices/{id}/actions", post(device_action))
        .layer(axum::middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

async fn contract() -> Json<serde_json::Value> {
    Json(crate::contract::schema_bundle())
}

async fn open_session(State(state): State<Arc<AppState>>) -> Response {
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict",
        state.cookie_name,
        state.guard.token()
    );
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

async fn snapshot(State(state): State<Arc<AppState>>) -> Json<crate::WorkbenchSnapshot> {
    Json(state.supervisor.snapshot().await)
}

async fn event_history(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<crate::EventHistoryPage>, ApiFailure> {
    state
        .supervisor
        .event_history(&query)
        .map(Json)
        .map_err(ApiFailure::from)
}

async fn connection_history(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<crate::ConnectionHistoryPage>, ApiFailure> {
    state
        .supervisor
        .connection_history(&query)
        .map(Json)
        .map_err(ApiFailure::from)
}

async fn device_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Json<crate::LogPage>, ApiFailure> {
    state
        .supervisor
        .logs(&id, query.cursor, query.limit)
        .await
        .map(Json)
        .map_err(ApiFailure::from)
}

async fn create_device(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateDeviceRequest>,
) -> Result<Json<crate::DeviceSnapshot>, ApiFailure> {
    let device = state
        .supervisor
        .add_device(request.id.clone(), request.label)
        .await?;
    if request.start {
        return state
            .supervisor
            .start_device(&request.id)
            .await
            .map(Json)
            .map_err(ApiFailure::from);
    }
    Ok(Json(device))
}

async fn device_action(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(action): Json<DeviceAction>,
) -> Result<Json<crate::DeviceSnapshot>, ApiFailure> {
    let result = match action {
        DeviceAction::Start => state.supervisor.start_device(&id).await,
        DeviceAction::Stop => state.supervisor.stop_device(&id).await,
        DeviceAction::Restart => state.supervisor.restart_device(&id).await,
        DeviceAction::ForceStop => state.supervisor.force_stop_device(&id).await,
    };
    result.map(Json).map_err(ApiFailure::from)
}

async fn events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.supervisor.subscribe()).map(|received| {
        let backend_event = match received {
            Ok(event) => event,
            Err(_) => BackendEvent {
                revision: 0,
                at_ms: 0,
                kind: EventKind::SnapshotRequired,
                device_id: None,
                message: "event consumer lagged; fetch a fresh snapshot".into(),
            },
        };
        let event = match Event::default()
            .event("workbench")
            .json_data(&backend_event)
        {
            Ok(event) => event,
            Err(error) => Event::default()
                .event("workbench_error")
                .data(format!("serialize event: {error}")),
        };
        Ok(event)
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn gate(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let host = header_text(headers, header::HOST);
    let origin = header_text(headers, header::ORIGIN);
    if let Err(refusal) = state.guard.check_origin(host, origin) {
        return refusal_response(refusal);
    }
    let bearer =
        header_text(headers, header::AUTHORIZATION).and_then(|value| value.strip_prefix("Bearer "));
    let cookie = header_text(headers, header::COOKIE)
        .and_then(|value| cookie_value(value, &state.cookie_name));
    if let Err(refusal) = state.guard.check_token(bearer.or(cookie)) {
        return refusal_response(refusal);
    }
    next.run(request).await
}

fn header_text(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn refusal_response(refusal: Refusal) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiError {
            error: "unauthorized",
            message: refusal.reason().to_owned(),
        }),
    )
        .into_response()
}

struct ApiFailure(SupervisorError);

impl From<SupervisorError> for ApiFailure {
    fn from(error: SupervisorError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            SupervisorError::Invalid(_) => StatusCode::BAD_REQUEST,
            SupervisorError::AlreadyExists(_) | SupervisorError::Conflict(_) => {
                StatusCode::CONFLICT
            }
            SupervisorError::NotFound(_) => StatusCode::NOT_FOUND,
            SupervisorError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = ApiError {
            error: self.0.code(),
            message: self.0.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn every_api_route_requires_the_loopback_credential() {
        let directory = tempfile::tempdir().expect("tempdir");
        let supervisor = Supervisor::new(
            directory.path().to_path_buf(),
            directory.path().join("missing-lait"),
        )
        .expect("supervisor");
        let app = router(supervisor, "test-token".into(), 7717);

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/workbench/snapshot")
                    .header("host", "127.0.0.1:7717")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .oneshot(
                Request::builder()
                    .uri("/api/workbench/snapshot")
                    .header("host", "127.0.0.1:7717")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn foreign_hosts_are_rejected_even_with_the_token() {
        let directory = tempfile::tempdir().expect("tempdir");
        let supervisor = Supervisor::new(
            directory.path().to_path_buf(),
            directory.path().join("missing-lait"),
        )
        .expect("supervisor");
        let app = router(supervisor, "test-token".into(), 7717);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/workbench/snapshot")
                    .header("host", "malicious.example")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn contract_endpoint_is_authenticated_and_versioned() {
        let directory = tempfile::tempdir().expect("tempdir");
        let supervisor = Supervisor::new(
            directory.path().to_path_buf(),
            directory.path().join("missing-lait"),
        )
        .expect("supervisor");
        let app = router(supervisor, "test-token".into(), 7717);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/workbench/contract")
                    .header("host", "127.0.0.1:7717")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let contract: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(contract["schemaVersion"], crate::SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn observability_routes_are_authenticated_and_bounded() {
        let directory = tempfile::tempdir().expect("tempdir");
        let supervisor = Supervisor::new(
            directory.path().to_path_buf(),
            directory.path().join("missing-lait"),
        )
        .expect("supervisor");
        supervisor
            .add_device("alice".into(), "Alice".into())
            .await
            .expect("add device");
        let app = router(supervisor, "test-token".into(), 7717);

        let history = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/workbench/history/events?afterRevision=0&limit=20")
                    .header("host", "127.0.0.1:7717")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(history.status(), StatusCode::OK);

        let logs = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/workbench/devices/alice/logs?limit=20")
                    .header("host", "127.0.0.1:7717")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(logs.status(), StatusCode::OK);

        let invalid_limit = app
            .oneshot(
                Request::builder()
                    .uri("/api/workbench/history/connections?limit=0")
                    .header("host", "127.0.0.1:7717")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(invalid_limit.status(), StatusCode::BAD_REQUEST);
    }
}
