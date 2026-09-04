//! `lait-snapshot-gateway` — the conditional write in front of the daemon-less
//! hosting bucket. Two routes:
//!
//! - `GET  /s/:key` — read the current snapshot (a convenience/health path; the
//!   real read is the bucket/CDN served directly, same origin).
//! - `PUT  /s/:key` — authorize and conditionally write a snapshot.
//!
//! `:key` is the object's capability basename ([`contact::gateway::object_key`]),
//! a one-way digest of the Space id. The PUT handler recomputes the key from the
//! envelope's declared Space and refuses any mismatch, so a signed write for one
//! Space can never be redirected onto another's object. All authority reasoning
//! lives in the library and `contact::gateway`; this file is arguments, routing,
//! and status codes.

mod gcs;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use lait_snapshot_gateway::{apply_write, read_snapshot, Gateway, ObjectStore, WriteOutcome};
use tower_http::cors::{Any, CorsLayer};

use crate::gcs::{metadata_token, GcsStore};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lait_snapshot_gateway=info".into()),
        )
        .init();

    let mut http: Option<SocketAddr> = None;
    let mut bucket: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => {
                http = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--http needs an address"))?
                        .parse()
                        .context("--http is not an address")?,
                );
            }
            "--bucket" => {
                bucket = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--bucket needs a name"))?,
                )
            }
            other => return Err(anyhow!("unknown argument: {other}")),
        }
    }
    // Cloud Run hands the port in $PORT; the flag wins for local runs.
    let http = http
        .or_else(|| {
            std::env::var("PORT")
                .ok()
                .and_then(|p| format!("0.0.0.0:{p}").parse().ok())
        })
        .ok_or_else(|| anyhow!("no listen address (--http or $PORT)"))?;
    let bucket = bucket
        .or_else(|| std::env::var("GATEWAY_BUCKET").ok())
        .ok_or_else(|| anyhow!("no bucket (--bucket or $GATEWAY_BUCKET)"))?;

    let store: Arc<dyn ObjectStore> = Arc::new(GcsStore::new(bucket.clone(), metadata_token));
    let gateway = Arc::new(Gateway { store });

    // The tab reads and writes from another origin. Authority is the signed
    // envelope, never the origin, so the allow-list is open: CORS gates which
    // page may READ the response in a browser, and nothing here trusts it for
    // anything. A forged write still fails `authorize_write`; a cross-origin
    // read is already public at the bucket.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    let app = Router::new()
        .route("/s/{key}", get(handle_get).put(handle_put))
        .route("/healthz", get(|| async { "ok" }))
        .layer(cors)
        .with_state(gateway);

    tracing::info!(%http, %bucket, "snapshot gateway up");
    let listener = tokio::net::TcpListener::bind(http).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Map a capability basename back to a full bucket key. The path segment is the
/// `<name>.snap` basename `object_key` produces; the key is `spaces/<that>`.
fn key_for(basename: &str) -> Result<String, StatusCode> {
    // Reject anything that is not a bare capability basename — no traversal, no
    // slashes, no wandering out of the snapshot prefix.
    if basename.is_empty()
        || basename.contains('/')
        || basename.contains("..")
        || basename.len() > 128
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(format!("spaces/{basename}"))
}

async fn handle_get(
    State(gateway): State<Arc<Gateway>>,
    Path(basename): Path<String>,
) -> Result<Bytes, StatusCode> {
    let key = key_for(&basename)?;
    match read_snapshot(gateway.store.as_ref(), &key) {
        Ok(Some(stored)) => Ok(Bytes::from(stored.bytes)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(why) => {
            tracing::warn!(%key, %why, "read failed");
            Err(StatusCode::BAD_GATEWAY)
        }
    }
}

async fn handle_put(
    State(gateway): State<Arc<Gateway>>,
    Path(basename): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let key = key_for(&basename).map_err(|s| (s, "bad object path".into()))?;

    // Bind the path to the envelope's Space: recompute the capability from the
    // declared Space and refuse a mismatch before any store access. This is the
    // anti-redirect check — the one thing the path alone cannot be trusted for.
    let envelope = contact::gateway::WriteEnvelope::decode(&body)
        .map_err(|why| (StatusCode::BAD_REQUEST, why))?;
    let expected_key = contact::gateway::object_key(&envelope.request.space);
    if expected_key != key {
        return Err((
            StatusCode::FORBIDDEN,
            "the write's Space does not own this object path".into(),
        ));
    }

    match apply_write(gateway.store.as_ref(), &key, &envelope.request.space, &body) {
        WriteOutcome::Accepted { generation } => {
            Ok((StatusCode::OK, format!("{{\"generation\":{generation}}}")))
        }
        WriteOutcome::Conflict { current } => Err((
            StatusCode::PRECONDITION_FAILED,
            format!("{{\"conflict\":true,\"current\":{current}}}"),
        )),
        WriteOutcome::Denied(denial) => Err((StatusCode::FORBIDDEN, denial.to_string())),
        WriteOutcome::BadRequest(why) => Err((StatusCode::BAD_REQUEST, why)),
        WriteOutcome::Unavailable(why) => {
            tracing::warn!(%key, %why, "store unavailable");
            Err((StatusCode::BAD_GATEWAY, "the store is unavailable".into()))
        }
    }
}
