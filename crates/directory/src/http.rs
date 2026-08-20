//! The HTTP surface, and the mapping from a refusal to a status code.
//!
//! In the library rather than a binary so a test can serve it over a real
//! socket — a routing table that only exists inside `main` is one nothing can
//! drive, and the status mapping below is exactly the kind of thing that is
//! obviously right and quietly wrong.
//!
//! # Why every route is under `/directory`
//!
//! This mounts beside `lait_post`'s router in one service, which is what
//! AUTH-25 chose when it put directory, carriage and registry on one thing
//! rather than three infrastructures. Both halves have a `/health` and a
//! `/challenge`, and `Router::merge` on a collision panics at startup rather
//! than at the first request — which would at least be loud, but only after a
//! deploy. The prefix means the two can never argue.

use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};

use crate::{Challenge, Refusal, Service, SignedPublish, SignedResolve, Store};

/// The service, shared across handlers.
pub type Shared<S> = Arc<Mutex<Service<S>>>;

/// Unix seconds. One place, so the handlers and the sweep agree.
#[must_use]
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn router<S: Store + Send + 'static>(shared: Shared<S>) -> Router {
    Router::new()
        .route("/directory/health", get(|| async { "ok" }))
        .route("/directory/challenge", get(challenge::<S>))
        .route("/directory/publish", post(publish::<S>))
        .route("/directory/resolve", post(resolve::<S>))
        .with_state(shared)
}

/// What a refusal looks like on the wire.
///
/// A separate enum rather than serializing [`Refusal`] directly, for one
/// reason: `Refusal::Unavailable` carries why the *store* could not answer, and
/// that string is for an operator reading logs, not for whoever is asking. A
/// prober who could read it would learn about the service's internals from a
/// request that was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "refusal")]
pub enum Refused {
    /// Not resolvable to you. Deliberately covers both "no such address" and
    /// "you may not ask" — see [`Refusal::NotAvailable`].
    NotAvailable,
    Malformed,
    NotAuthentic,
    StaleChallenge,
    TooFast,
    TooLarge,
    Unavailable,
}

impl From<&Refusal> for Refused {
    fn from(refusal: &Refusal) -> Self {
        match refusal {
            Refusal::NotAvailable => Self::NotAvailable,
            Refusal::Malformed => Self::Malformed,
            Refusal::NotAuthentic => Self::NotAuthentic,
            Refusal::StaleChallenge => Self::StaleChallenge,
            Refusal::TooFast => Self::TooFast,
            Refusal::TooLarge => Self::TooLarge,
            Refusal::Unavailable(_) => Self::Unavailable,
        }
    }
}

/// A refusal keeps its name across the wire. Every arm names a different
/// remedy, and collapsing them would make "sign it properly" and "ask for a new
/// challenge" the same message.
fn refused(refusal: &Refusal) -> (StatusCode, Json<Refused>) {
    let status = match refusal {
        // 404 for both absence and denial. The status is part of the answer, so
        // a distinguishable one here would undo what the refusal value is
        // careful about.
        Refusal::NotAvailable => StatusCode::NOT_FOUND,
        Refusal::Malformed => StatusCode::BAD_REQUEST,
        Refusal::NotAuthentic => StatusCode::FORBIDDEN,
        Refusal::StaleChallenge => StatusCode::CONFLICT,
        Refusal::TooFast => StatusCode::TOO_MANY_REQUESTS,
        Refusal::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        Refusal::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(Refused::from(refusal)))
}

fn held<S: Store>(shared: &Shared<S>) -> std::sync::MutexGuard<'_, Service<S>> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Deserialize)]
struct DeviceQuery {
    device: String,
}

/// Issue a challenge. Free and unauthenticated, for anyone, about any device.
async fn challenge<S: Store>(
    State(shared): State<Shared<S>>,
    Query(query): Query<DeviceQuery>,
) -> Result<Json<Challenge>, (StatusCode, Json<Refused>)> {
    // Parsed rather than trusted: the canonicality check inside the service is
    // what keeps one key from being two entries, and a query string is exactly
    // where a re-spelling arrives.
    let Some(device) = DeviceId::parse(&query.device) else {
        return Err(refused(&Refusal::Malformed));
    };
    let issued = held(&shared)
        .challenge(&device, now())
        .map_err(|refusal| refused(&refusal))?;
    Ok(Json(issued))
}

/// The address a publication answers with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Published {
    pub address: String,
}

async fn publish<S: Store>(
    State(shared): State<Shared<S>>,
    Json(request): Json<SignedPublish>,
) -> Result<Json<Published>, (StatusCode, Json<Refused>)> {
    let address = held(&shared)
        .publish(&request, now())
        .map_err(|refusal| refused(&refusal))?;
    Ok(Json(Published {
        address: address.as_str().to_owned(),
    }))
}

/// What a resolution answers with: the announcement exactly as its publisher
/// encoded it, hex-encoded for transport.
///
/// Bytes rather than a decoded value, because the *reader* is the party that
/// must anchor them. A service that handed back a parsed device set would be
/// inviting a caller to trust its parsing, which is the one thing this design
/// spends its whole structure avoiding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolved {
    pub announcement: String,
}

async fn resolve<S: Store>(
    State(shared): State<Shared<S>>,
    Json(request): Json<SignedResolve>,
) -> Result<Json<Resolved>, (StatusCode, Json<Refused>)> {
    let bytes = held(&shared)
        .resolve(&request, now())
        .map_err(|refusal| refused(&refusal))?;
    Ok(Json(Resolved {
        announcement: data_encoding::HEXLOWER.encode(&bytes),
    }))
}
