//! The notify relay: the instant half of the update seam.
//!
//! The feed is a bucket of signed objects, and a bucket cannot tell anyone
//! anything. Every installed machine polls it on a period, which is the floor
//! and stays the floor. This relay is what makes the period irrelevant in the
//! ordinary case: the publisher hands it the same signed pointer it just
//! wrote to the bucket, and every daemon holding a subscription hears within
//! the round trip.
//!
//! ## What it is not
//!
//! It is not an authority. A pointer arrives here already sealed by the feed
//! key; the relay verifies it with the same primitive the daemon does and
//! refuses anything else, so the worst a hostile announcer can do is spend a
//! signature check. It cannot mint a pointer, move a channel, or roll one
//! back — `published_at` only ratchets forward here, exactly as it does on
//! every machine. A daemon that hears something from this relay still walks
//! the bucket, the manifest, and the digests before a byte is unpacked; the
//! only thing that changed is *when* it walked.
//!
//! ## Running one
//!
//! ```sh
//! lait-feed-notify --http 127.0.0.1:8095 --state /var/lib/lait-feed-notify/board.json \
//!   --pubkey <feed key hex> [--pubkey <successor hex>]
//! ```
//!
//! It holds no keys of its own and terminates no TLS: put it behind something
//! that does. Which keys it relays for is the whole of its configuration, so an
//! operator running their own channel runs their own relay for their own key
//! and points `update.notify` at it.
//!
//! - `POST /announce/<key>` — body is the signed envelope; `202` when it was
//!   newer than what the board held and has been fanned out, `409` when not.
//! - `GET /subscribe` — SSE. The newest pointer per key is replayed first, so
//!   a daemon reconnecting after a gap catches up without a poll; then live.
//! - `GET /latest/<key>` — the envelope the board holds, for a curl check.
//! - `GET /health`. (`/healthz` specifically is intercepted by Google's
//!   frontend on Cloud Run and never reaches a container; the Post learned
//!   the same.)
//!
//! `<key>` is `stable`, `test`, or `worlds/<world id>/<stable|test>` — the
//! same path the pointer lives at under `channels/` in the bucket, so a key
//! *is* the object's name and nothing has to map between them.
//!
//! ## Priming: the board is a cache of the bucket, never a second record
//!
//! With `--feed <base>` the relay reads every channel pointer the bucket
//! holds when it starts and again every `--prime-every` seconds (and on a
//! subscribe that finds the board stale), taking each through the same
//! verify-and-ratchet path an announce takes. So a restart remembers
//! nothing and loses nothing, a publish whose announce never arrived is
//! still heard within minutes rather than hours, and the bucket stays the
//! only authority — the relay only ever repeats what it verified there.
//! `--state` remains for an operator whose feed is not a listable bucket.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// The most an envelope may be. A pointer is a few hundred bytes; this is
/// generous and still far below anything worth a body-parsing attack.
pub const MAX_ENVELOPE_BYTES: usize = 16 * 1024;

/// How often an idle subscription hears a comment frame, so a proxy between
/// here and a daemon does not decide the connection is dead.
pub const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// How often the board is re-read from the feed by default: the bound on
/// how long a publish whose announce was lost stays unheard.
pub const PRIME_EVERY: Duration = Duration::from_secs(300);

/// The most a pointer object may be when primed from the feed. A pointer is a
/// few hundred bytes; the bound is against a bucket that serves the wrong
/// object, not against the publisher.
const MAX_POINTER_BYTES: u64 = MAX_ENVELOPE_BYTES as u64;

/// How many announcements a slow subscriber may fall behind before it is
/// skipped ahead. Every announcement is a full pointer, so skipping loses
/// nothing but history a reconnect would have replayed anyway.
const FANOUT_DEPTH: usize = 64;

/// One announced pointer: the key it was announced under and the envelope
/// exactly as it was received, since the bytes are what the signature covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    pub key: String,
    pub envelope: serde_json::Value,
    pub published_at: u64,
}

/// Why an announcement was refused. Each is its own status at the edge.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The key is not a channel path this relay recognises.
    BadKey(String),
    /// The body is not an envelope, or its payload is not a pointer.
    Malformed(String),
    /// The envelope verifies against none of the relay's keys.
    Unverified,
    /// The pointer carries no `published_at`; nothing unstamped travels here.
    Unstamped,
    /// The board already holds this or something newer under that key.
    NotNewer { held: u64 },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::BadKey(key) => write!(f, "`{key}` is not a channel key"),
            Refusal::Malformed(why) => write!(f, "not a pointer envelope: {why}"),
            Refusal::Unverified => {
                write!(f, "the envelope verifies against none of the relay's keys")
            }
            Refusal::Unstamped => write!(f, "the pointer carries no published_at"),
            Refusal::NotNewer { held } => {
                write!(f, "the board already holds a pointer published at {held}")
            }
        }
    }
}

/// The board: the newest verified pointer per key, and the fan-out behind it.
pub struct Board {
    pubkeys: Vec<[u8; 32]>,
    latest: BTreeMap<String, Announcement>,
    state: Option<PathBuf>,
    fanout: broadcast::Sender<Announcement>,
    /// The feed this board is a cache of, when it is one.
    feed: Option<String>,
    /// When the feed was last read whole, so a subscribe can tell stale.
    primed_at: Option<std::time::Instant>,
    /// Set while a prime is running, so two subscribers arriving at once do
    /// not start two.
    priming: bool,
}

/// What one pass over the feed came to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Primed {
    /// Keys whose pointer moved the board.
    pub moved: Vec<String>,
    /// Keys read and found no newer than held.
    pub unchanged: usize,
    /// Keys that could not be read or were refused, with why.
    pub failed: Vec<(String, String)>,
}

pub type Shared = Arc<Mutex<Board>>;

#[derive(Deserialize)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Deserialize)]
struct Stamped {
    #[serde(default)]
    published_at: Option<u64>,
}

impl Board {
    /// A board relaying for `pubkeys`, restored from `state` when it names a
    /// file that exists. An empty key set is refused: a relay that verifies
    /// against nothing would relay anything, while looking configured.
    pub fn open(pubkeys: Vec<[u8; 32]>, state: Option<PathBuf>) -> Result<Board> {
        if pubkeys.is_empty() {
            return Err(anyhow!(
                "a relay needs at least one --pubkey to verify against"
            ));
        }
        let latest = match &state {
            Some(path) if path.exists() => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("read the board at {}", path.display()))?;
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("the board at {} is not readable", path.display()))?
            }
            _ => BTreeMap::new(),
        };
        let (fanout, _) = broadcast::channel(FANOUT_DEPTH);
        Ok(Board {
            pubkeys,
            latest,
            state,
            fanout,
            feed: None,
            primed_at: None,
            priming: false,
        })
    }

    /// Make this board a cache of `feed` — the base the channel objects live
    /// under, `https://storage.googleapis.com/<bucket>` for the Foundation's.
    pub fn with_feed(mut self, feed: &str) -> Board {
        self.feed = Some(feed.trim_end_matches('/').to_owned());
        self
    }

    /// The feed this board caches, if any.
    pub fn feed(&self) -> Option<&str> {
        self.feed.as_deref()
    }

    /// Whether the board has gone `every` without a prime, or never primed.
    pub fn stale(&self, every: Duration) -> bool {
        self.feed.is_some()
            && !self.priming
            && self.primed_at.is_none_or(|at| at.elapsed() >= every)
    }

    /// Verify and take an announcement, fanning it out when it is newer than
    /// what the board holds under that key.
    pub fn announce(&mut self, key: &str, body: &[u8]) -> Result<Announcement, Refusal> {
        if !is_channel_key(key) {
            return Err(Refusal::BadKey(key.to_owned()));
        }
        let envelope: Envelope = serde_json::from_slice(body)
            .map_err(|error| Refusal::Malformed(format!("envelope: {error}")))?;
        let payload = data_encoding::BASE64
            .decode(envelope.payload.as_bytes())
            .map_err(|error| Refusal::Malformed(format!("payload base64: {error}")))?;
        let signature: [u8; 64] = data_encoding::BASE64
            .decode(envelope.signature.as_bytes())
            .map_err(|error| Refusal::Malformed(format!("signature base64: {error}")))?
            .try_into()
            .map_err(|_| Refusal::Malformed("signature is not 64 bytes".into()))?;
        if !self
            .pubkeys
            .iter()
            .any(|key| mechanics::actor::verify_detached(key, &payload, &signature))
        {
            return Err(Refusal::Unverified);
        }
        let stamped: Stamped = serde_json::from_slice(&payload)
            .map_err(|error| Refusal::Malformed(format!("pointer payload: {error}")))?;
        let published_at = stamped.published_at.ok_or(Refusal::Unstamped)?;
        if let Some(held) = self.latest.get(key) {
            if held.published_at >= published_at {
                return Err(Refusal::NotNewer {
                    held: held.published_at,
                });
            }
        }
        // Re-encoded from the parsed envelope rather than the raw body, so a
        // subscriber receives one JSON object on one line whatever whitespace
        // the announcer sent. The payload and signature strings are untouched,
        // which is all the signature covers.
        let announcement = Announcement {
            key: key.to_owned(),
            envelope: serde_json::json!({
                "payload": envelope.payload,
                "signature": envelope.signature,
            }),
            published_at,
        };
        self.latest.insert(key.to_owned(), announcement.clone());
        self.persist();
        // No subscriber is not an error; the board still moved.
        let _ = self.fanout.send(announcement.clone());
        Ok(announcement)
    }

    /// What the board holds under `key`.
    pub fn latest(&self, key: &str) -> Option<&Announcement> {
        self.latest.get(key)
    }

    /// Everything the board holds, for a subscriber's replay.
    pub fn snapshot(&self) -> Vec<Announcement> {
        self.latest.values().cloned().collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<Announcement> {
        self.fanout.subscribe()
    }

    /// Best effort, and said when it fails: a board that cannot persist still
    /// relays, and loses only its replay across a restart — which the poll
    /// floor covers. Failing the announcement over it would trade an instant
    /// update for a disk fault nobody is watching.
    fn persist(&self) {
        let Some(path) = &self.state else {
            return;
        };
        let write = (|| -> Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temp = path.with_extension("json.tmp");
            std::fs::write(&temp, serde_json::to_vec_pretty(&self.latest)?)?;
            std::fs::rename(&temp, path)?;
            Ok(())
        })();
        if let Err(error) = write {
            tracing::warn!(%error, path = %path.display(), "the board could not be persisted");
        }
    }
}

/// The channel keys a feed holds. A feed on Google Cloud Storage is listed
/// through its public JSON API, so a World published since the relay started
/// is discovered rather than configured; any other feed yields the product's
/// two keys, and its Worlds are heard through announces alone.
pub fn discover_keys(feed: &str) -> Vec<String> {
    let mut keys = vec!["stable".to_owned(), "test".to_owned()];
    let Some(bucket) = feed
        .strip_prefix("https://storage.googleapis.com/")
        .map(|rest| rest.trim_matches('/'))
        .filter(|bucket| !bucket.is_empty() && !bucket.contains('/'))
    else {
        return keys;
    };
    #[derive(Deserialize)]
    struct Listing {
        #[serde(default)]
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        name: String,
    }
    let url = format!(
        "https://storage.googleapis.com/storage/v1/b/{bucket}/o?prefix=channels/&fields=items(name)"
    );
    let listing: Result<Listing> = (|| {
        let response = ureq::get(&url)
            .timeout(Duration::from_secs(30))
            .call()
            .context("list the feed's channels")?;
        serde_json::from_reader::<_, Listing>(response.into_reader()).context("channel listing")
    })();
    match listing {
        Ok(listing) => {
            for item in listing.items {
                if let Some(key) = item.name.strip_prefix("channels/") {
                    if is_channel_key(key) && !keys.iter().any(|held| held == key) {
                        keys.push(key.to_owned());
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "the feed could not be listed; priming the product keys only")
        }
    }
    keys
}

/// Read every channel pointer the feed holds through the board's own
/// announce path. Blocking; a pass is a handful of small GETs.
pub fn prime(board: &Shared, every_key: &[String]) -> Primed {
    let feed = {
        let mut board = lock(board);
        let Some(feed) = board.feed.clone() else {
            return Primed::default();
        };
        board.priming = true;
        feed
    };
    let mut primed = Primed::default();
    for key in every_key {
        let url = format!("{feed}/channels/{key}");
        let fetched: Result<Vec<u8>> = (|| {
            use std::io::Read;
            let response = ureq::get(&url).timeout(Duration::from_secs(30)).call()?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_POINTER_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_POINTER_BYTES {
                anyhow::bail!("the pointer object exceeds {MAX_POINTER_BYTES} bytes");
            }
            Ok(bytes)
        })();
        let bytes = match fetched {
            Ok(bytes) => bytes,
            Err(error) => {
                // A channel with nothing published yet is a 404 and not a
                // fault; anything else is.
                let why = error.to_string();
                if !why.contains("status code 404") {
                    primed.failed.push((key.clone(), why));
                }
                continue;
            }
        };
        match lock(board).announce(key, &bytes) {
            Ok(_) => primed.moved.push(key.clone()),
            Err(Refusal::NotNewer { .. }) => primed.unchanged = primed.unchanged.saturating_add(1),
            Err(refusal) => primed.failed.push((key.clone(), refusal.to_string())),
        }
    }
    let mut board = lock(board);
    board.priming = false;
    board.primed_at = Some(std::time::Instant::now());
    primed
}

/// One whole prime: discover, then read. On the blocking pool, so the SSE
/// streams and announces are never held behind a GET.
pub async fn prime_now(board: Shared) -> Primed {
    let feed = lock(&board).feed.clone();
    let Some(feed) = feed else {
        return Primed::default();
    };
    let primed_board = board.clone();
    tokio::task::spawn_blocking(move || {
        let keys = discover_keys(&feed);
        let primed = prime(&primed_board, &keys);
        if primed.moved.is_empty() && primed.failed.is_empty() {
            tracing::debug!(unchanged = primed.unchanged, "the feed holds nothing newer");
        } else {
            tracing::info!(moved = ?primed.moved, failed = ?primed.failed, unchanged = primed.unchanged, "primed from the feed");
        }
        primed
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "the prime panicked");
        lock(&board).priming = false;
        Primed::default()
    })
}

/// Prime on start and then every `every`, for the process's life.
pub async fn serve_priming(board: Shared, every: Duration) {
    loop {
        prime_now(board.clone()).await;
        tokio::time::sleep(every).await;
    }
}

/// `stable`, `test`, or `worlds/<world>/<stable|test>`. A World id is what
/// `world.json` may carry: lowercase reverse-DNS, nothing that could walk a
/// path or hide a second key inside one.
pub fn is_channel_key(key: &str) -> bool {
    fn is_segment(s: &str) -> bool {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'_'
            })
    }
    let is_channel = |s: &str| s == "stable" || s == "test";
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        [channel] => is_channel(channel),
        ["worlds", world, channel] => is_segment(world) && is_channel(channel),
        _ => false,
    }
}

/// Decode `--pubkey` hex into the raw key, refusing a duplicate for the same
/// reason the daemon does: a repeated key hides a replacement.
pub fn parse_pubkeys(hex: &[String]) -> Result<Vec<[u8; 32]>> {
    let mut keys = Vec::with_capacity(hex.len());
    for text in hex {
        let bytes = data_encoding::HEXLOWER
            .decode(text.trim().as_bytes())
            .with_context(|| format!("--pubkey {text} is not lowercase hex"))?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("--pubkey {text} is not 32 bytes"))?;
        if keys.contains(&key) {
            return Err(anyhow!("--pubkey {text} appears twice"));
        }
        keys.push(key);
    }
    Ok(keys)
}

/// The relay's routes over a shared board. A subscribe that finds the board
/// stale (no prime for `prime_every`) starts one, so a board that has been
/// idle — CPU-throttled with nobody connected — is fresh for who just came.
pub fn router(board: Shared) -> Router {
    router_priming(board, PRIME_EVERY)
}

/// [`router`] with the staleness bound supplied.
pub fn router_priming(board: Shared, prime_every: Duration) -> Router {
    let subscribe_state = (board.clone(), prime_every);
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/subscribe",
            get(move || {
                let (board, prime_every) = subscribe_state.clone();
                async move {
                    if lock(&board).stale(prime_every) {
                        tokio::spawn(prime_now(board.clone()));
                    }
                    subscribe(board).await
                }
            }),
        )
        .route("/latest/{*key}", get(latest))
        .route(
            "/announce/{*key}",
            post(announce).layer(DefaultBodyLimit::max(MAX_ENVELOPE_BYTES)),
        )
        .with_state(board)
}

fn lock(board: &Shared) -> std::sync::MutexGuard<'_, Board> {
    board
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn announce(
    State(board): State<Shared>,
    Path(key): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    match lock(&board).announce(&key, &body) {
        Ok(announcement) => {
            tracing::info!(key = %announcement.key, published_at = announcement.published_at, "pointer announced");
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "key": announcement.key,
                    "published_at": announcement.published_at,
                })),
            )
        }
        Err(refusal) => {
            let status = match refusal {
                Refusal::BadKey(_) => StatusCode::NOT_FOUND,
                Refusal::Malformed(_) | Refusal::Unstamped => StatusCode::BAD_REQUEST,
                Refusal::Unverified => StatusCode::FORBIDDEN,
                Refusal::NotNewer { .. } => StatusCode::CONFLICT,
            };
            tracing::debug!(%key, %refusal, "announcement refused");
            (
                status,
                Json(serde_json::json!({ "refused": refusal.to_string() })),
            )
        }
    }
}

async fn latest(State(board): State<Shared>, Path(key): Path<String>) -> impl IntoResponse {
    match lock(&board).latest(&key) {
        Some(announcement) => (StatusCode::OK, Json(announcement.envelope.clone())),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "refused": "nothing has been announced under that key" })),
        ),
    }
}

/// The SSE stream: replay, then live. Both are taken under one lock so an
/// announcement cannot land between the snapshot and the subscription and be
/// missed by a subscriber that arrived at exactly the wrong moment.
async fn subscribe(
    board: Shared,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let (replay, live) = {
        let board = lock(&board);
        (board.snapshot(), board.subscribe())
    };
    let replay = tokio_stream::iter(replay.into_iter().map(Ok));
    let live = BroadcastStream::new(live).filter_map(|received| received.ok().map(Ok));
    let stream = replay
        .chain(live)
        .map(|announcement: Result<Announcement, Infallible>| {
            let announcement = announcement.unwrap_or_else(|never| match never {});
            Ok(Event::default()
                .event("pointer")
                .json_data(&announcement)
                .unwrap_or_else(|error| Event::default().event("error").data(error.to_string())))
        });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE).text("hb"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair(seed_byte: u8) -> ([u8; 32], [u8; 32]) {
        let seed = [seed_byte; 32];
        let device = mechanics::actor::device_from_seed(&seed);
        let pubkey: [u8; 32] = data_encoding::HEXLOWER
            .decode(device.as_str().as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        (seed, pubkey)
    }

    fn sealed(seed: &[u8; 32], payload: serde_json::Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(&payload).unwrap();
        let signature = mechanics::actor::sign_detached(seed, &bytes);
        serde_json::json!({
            "payload": data_encoding::BASE64.encode(&bytes),
            "signature": data_encoding::BASE64.encode(&signature),
        })
        .to_string()
        .into_bytes()
    }

    fn pointer(version: &str, published_at: Option<u64>) -> serde_json::Value {
        let mut value = serde_json::json!({
            "kind": "release",
            "version": version,
            "manifest": format!("https://feed.example/releases/{version}/manifest.json"),
        });
        if let Some(at) = published_at {
            value["published_at"] = at.into();
        }
        value
    }

    #[test]
    fn a_key_is_a_channel_path_and_nothing_else() {
        for ok in [
            "stable",
            "test",
            "worlds/com.lait.issues/stable",
            "worlds/com.lait.signage/test",
        ] {
            assert!(is_channel_key(ok), "{ok} is a channel key");
        }
        for bad in [
            "",
            "nightly",
            "worlds",
            "worlds/com.lait.issues",
            "worlds/../stable",
            "worlds/Com.Lait/stable",
            "worlds/com.lait.issues/stable/extra",
            "channels/stable",
            "stable/",
        ] {
            assert!(!is_channel_key(bad), "{bad} is not a channel key");
        }
    }

    /// The board is a ratchet: a pointer is taken only when it verifies and is
    /// newer than what is held, and each refusal keeps its own kind so the edge
    /// can say which.
    #[test]
    fn the_board_takes_only_verified_newer_stamped_pointers() {
        let (seed, pubkey) = keypair(7);
        let (other_seed, _) = keypair(9);
        let mut board = Board::open(vec![pubkey], None).unwrap();

        let first = board
            .announce("stable", &sealed(&seed, pointer("0.9.8", Some(100))))
            .expect("a verified stamped pointer is taken");
        assert_eq!(first.published_at, 100);
        assert_eq!(board.latest("stable").map(|a| a.published_at), Some(100));

        assert_eq!(
            board.announce("stable", &sealed(&seed, pointer("0.9.8", Some(100)))),
            Err(Refusal::NotNewer { held: 100 }),
            "the same pointer again is not newer"
        );
        assert_eq!(
            board.announce("stable", &sealed(&seed, pointer("0.9.7", Some(50)))),
            Err(Refusal::NotNewer { held: 100 }),
            "an older pointer cannot roll the board back"
        );
        assert_eq!(
            board.announce("stable", &sealed(&other_seed, pointer("0.9.9", Some(200)))),
            Err(Refusal::Unverified),
            "a pointer under a key the relay does not hold is refused"
        );
        assert_eq!(
            board.announce("stable", &sealed(&seed, pointer("0.9.9", None))),
            Err(Refusal::Unstamped),
            "an unstamped pointer cannot travel here"
        );
        assert_eq!(
            board.announce("nightly", &sealed(&seed, pointer("0.9.9", Some(200)))),
            Err(Refusal::BadKey("nightly".into()))
        );
        assert!(matches!(
            board.announce("stable", b"not json"),
            Err(Refusal::Malformed(_))
        ));

        let newer = board
            .announce("stable", &sealed(&seed, pointer("0.9.9", Some(200))))
            .expect("a newer pointer moves the board");
        assert_eq!(newer.published_at, 200);
        // Keys are independent ratchets.
        board
            .announce(
                "worlds/com.lait.signage/stable",
                &sealed(&seed, pointer("0.1.3", Some(150))),
            )
            .expect("a World's key ratchets on its own");
        assert_eq!(board.snapshot().len(), 2);
    }

    #[test]
    fn a_relay_with_no_keys_is_refused_rather_than_open() {
        assert!(Board::open(vec![], None).is_err());
        assert!(parse_pubkeys(&["zz".into()]).is_err());
        let hex = "227e448a16c19623707a3da8b8af6e1f70afcf18fb4e509e82115ef797666ba9".to_string();
        assert!(
            parse_pubkeys(&[hex.clone(), hex]).is_err(),
            "a duplicate hides a replacement"
        );
    }

    /// What was announced survives a restart, so a daemon reconnecting after
    /// the relay was replaced still hears the newest pointer on subscribe.
    #[test]
    fn the_board_persists_across_a_restart() {
        let (seed, pubkey) = keypair(7);
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("board").join("board.json");
        {
            let mut board = Board::open(vec![pubkey], Some(state.clone())).unwrap();
            board
                .announce("test", &sealed(&seed, pointer("0.9.8", Some(100))))
                .unwrap();
        }
        let mut reopened = Board::open(vec![pubkey], Some(state)).unwrap();
        assert_eq!(reopened.latest("test").map(|a| a.published_at), Some(100));
        assert_eq!(
            reopened
                .announce("test", &sealed(&seed, pointer("0.9.8", Some(100))))
                .map(|_| ()),
            Err(Refusal::NotNewer { held: 100 }),
            "the ratchet is restored with the board"
        );
    }
}
