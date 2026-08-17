//! The HTTP surface: three endpoints, and the mapping from a refusal to a
//! status code.
//!
//! It lives in the library rather than the binary so a test can serve it over a
//! real socket. A routing table that only exists inside `main` is one nothing
//! can drive, and the status mapping below is exactly the kind of thing that is
//! obviously right and quietly wrong.

use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use mechanics::ids::DeviceId;
use serde::Deserialize;

use crate::{Challenge, Deposited, FsStore, Post, Refusal, SignedAck, SignedDeposit, SignedFetch};

/// The Post, shared across handlers.
pub type Shared = Arc<Mutex<Post<FsStore>>>;

/// Unix seconds. One place, so the handlers and the sweep agree.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn router(shared: Shared) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/challenge", get(challenge))
        .route("/deposit", post(deposit))
        .route("/fetch", post(fetch))
        .route("/acknowledge", post(acknowledge))
        .with_state(shared)
}

/// A refusal keeps its name across the wire. Every arm names a different
/// remedy, and collapsing them into one status would make "sign it properly"
/// and "ask for a new challenge" the same message.
fn refused(refusal: Refusal) -> (StatusCode, Json<Refusal>) {
    let status = match refusal {
        Refusal::BadSignature => StatusCode::FORBIDDEN,
        Refusal::UnusableDevice | Refusal::UnusableExpiry => StatusCode::BAD_REQUEST,
        Refusal::UnknownChallenge | Refusal::ChallengeExpired => StatusCode::CONFLICT,
        Refusal::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        Refusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(refusal))
}

fn held(shared: &Shared) -> std::sync::MutexGuard<'_, Post<FsStore>> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Deserialize)]
struct DeviceQuery {
    device: String,
}

async fn challenge(
    State(shared): State<Shared>,
    Query(query): Query<DeviceQuery>,
) -> Result<Json<Challenge>, (StatusCode, Json<Refusal>)> {
    // Verbatim on purpose. `from_key_string` validates nothing, and that is what
    // is wanted here: the query string reaches `Post::challenge`'s canonicality
    // check exactly as it was spelled. Parsing here would *normalise* it — which
    // would silently accept a second spelling of one device and hand back a
    // challenge answerable under an id the store never saw.
    let device = DeviceId::from_key_string(query.device);
    held(&shared)
        .challenge(&device, now())
        .map(Json)
        .map_err(refused)
}

async fn deposit(
    State(shared): State<Shared>,
    Json(request): Json<SignedDeposit>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<Refusal>)> {
    held(&shared)
        .deposit(&request, now())
        .map(|id| Json(serde_json::json!({ "deposited": id })))
        .map_err(refused)
}

async fn fetch(
    State(shared): State<Shared>,
    Json(request): Json<SignedFetch>,
) -> Result<Json<Vec<Deposited>>, (StatusCode, Json<Refusal>)> {
    held(&shared)
        .fetch(&request, now())
        .map(Json)
        .map_err(refused)
}

async fn acknowledge(
    State(shared): State<Shared>,
    Json(request): Json<SignedAck>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<Refusal>)> {
    held(&shared)
        .acknowledge(&request, now())
        .map(|dropped| Json(serde_json::json!({ "dropped": dropped })))
        .map_err(refused)
}
