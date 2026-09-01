//! The HTTP surface: three endpoints, and the mapping from a refusal to a
//! status code.
//!
//! It lives in the library rather than the binary so a test can serve it over a
//! real socket. A routing table that only exists inside `main` is one nothing
//! can drive, and the status mapping below is exactly the kind of thing that is
//! obviously right and quietly wrong.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use mechanics::ids::DeviceId;
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::store::BoxedStore;
use crate::{
    Challenge, Deposited, Post, Refusal, SignedAck, SignedBlock, SignedDeposit, SignedFetch,
};

/// The Post, shared across handlers, over whichever store boot chose.
pub type Shared = Arc<Mutex<Post<BoxedStore>>>;

/// The wake doorbell: a per-device broadcast that something was deposited.
///
/// Value-free by construction — a subscriber still collects over the signed
/// path, so ringing carries nothing and proves nothing; it only spares the
/// poll. Knowing a device id already lets anyone *deposit*; letting it hear
/// "your mailbox moved" reveals strictly less than that.
#[derive(Clone, Default)]
pub struct Wake {
    bells: Arc<Mutex<HashMap<DeviceId, broadcast::Sender<()>>>>,
}

impl Wake {
    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<DeviceId, broadcast::Sender<()>>> {
        match self.bells.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn ring(&self, device: &DeviceId) {
        if let Some(bell) = self.locked().get(device) {
            let _ = bell.send(());
        }
    }

    fn subscribe(&self, device: DeviceId) -> broadcast::Receiver<()> {
        self.locked()
            .entry(device)
            .or_insert_with(|| broadcast::channel(4).0)
            .subscribe()
    }
}

/// Everything the handlers share.
#[derive(Clone)]
struct AppState {
    post: Shared,
    wake: Wake,
}

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
        .route("/block", post(block))
        .route("/wake", get(wake))
        .route("/sweep", post(sweep))
        .with_state(AppState {
            post: shared,
            wake: Wake::default(),
        })
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
        // 429 rather than 503: the remedy is to ask again later, and the caller
        // should not be able to tell a mailbox that is full from a sender that is
        // going too fast. One status for one remedy, which is the whole reason
        // this arm is coarse.
        Refusal::AtCapacity => StatusCode::TOO_MANY_REQUESTS,
    };
    (status, Json(refusal))
}

fn held(shared: &Shared) -> std::sync::MutexGuard<'_, Post<BoxedStore>> {
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
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
) -> Result<Json<Challenge>, (StatusCode, Json<Refusal>)> {
    // Verbatim on purpose. `from_key_string` validates nothing, and that is what
    // is wanted here: the query string reaches `Post::challenge`'s canonicality
    // check exactly as it was spelled. Parsing here would *normalise* it — which
    // would silently accept a second spelling of one device and hand back a
    // challenge answerable under an id the store never saw.
    let device = DeviceId::from_key_string(query.device);
    held(&state.post)
        .challenge(&device, now())
        .map(Json)
        .map_err(refused)
}

async fn deposit(
    State(state): State<AppState>,
    Json(request): Json<SignedDeposit>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<Refusal>)> {
    let recipient = request.envelope.recipient.clone();
    let deposited = held(&state.post)
        .deposit(&request, now())
        .map_err(refused)?;
    // Rung after the deposit is durable, so a woken subscriber that collects
    // immediately finds what it was woken for.
    state.wake.ring(&recipient);
    Ok(Json(serde_json::json!({ "deposited": deposited })))
}

async fn fetch(
    State(state): State<AppState>,
    Json(request): Json<SignedFetch>,
) -> Result<Json<Vec<Deposited>>, (StatusCode, Json<Refusal>)> {
    held(&state.post)
        .fetch(&request, now())
        .map(Json)
        .map_err(refused)
}

async fn acknowledge(
    State(state): State<AppState>,
    Json(request): Json<SignedAck>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<Refusal>)> {
    held(&state.post)
        .acknowledge(&request, now())
        .map(|dropped| Json(serde_json::json!({ "dropped": dropped })))
        .map_err(refused)
}

async fn block(
    State(state): State<AppState>,
    Json(request): Json<SignedBlock>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<Refusal>)> {
    held(&state.post)
        .block(&request, now())
        // An empty object rather than nothing: a body a client can decode is a
        // reply it can tell apart from a truncated one.
        .map(|()| Json(serde_json::json!({ "blocked": true })))
        .map_err(refused)
}

/// One standing wake stream for a device: an event whenever something is
/// deposited for it. Content-free — the collect that follows is what carries
/// letters, over the signed path. Unauthenticated on purpose: hearing "your
/// mailbox moved" reveals less than the unauthenticated deposit route already
/// accepts, and gating it would put a signature on a doorbell.
async fn wake(
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let bell = state
        .wake
        .subscribe(DeviceId::from_key_string(query.device));
    // A lagged receiver is still "something happened" — the event carries no
    // value, so collapsing a burst into one wake is exactly right.
    let mail = tokio_stream::StreamExt::filter_map(
        tokio_stream::wrappers::BroadcastStream::new(bell),
        |_| Some(Ok(Event::default().event("mail").data(""))),
    );
    let ready = tokio_stream::once(Ok(Event::default().event("ready").data("")));
    let stream = tokio_stream::StreamExt::chain(ready, mail);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Drop everything past its retention window. Safe for anyone to trigger —
/// it removes only material the store already refuses to deliver — which is
/// what lets a scheduler hit it without a credential when the service scales
/// to zero between requests.
async fn sweep(State(state): State<AppState>) -> Json<serde_json::Value> {
    let collected = held(&state.post).sweep(now());
    Json(serde_json::json!({ "swept": collected }))
}
