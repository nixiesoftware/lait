#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "HTTP range/chunk arithmetic is validated against declared and observed lengths; conversions preserve the existing content envelope and header encodings."
)]

//! Content over the local web surface: HEAD, ranged download, streaming upload.
//!
//! The browser's half of the content plane. Everything here is a thin, bounded
//! translation between HTTP and the daemon's content envelope — this module
//! decides nothing about what content *is*, and holds no bytes it is not
//! actively moving.
//!
//! Three properties shape every route below.
//!
//! **The bytes never accumulate here.** A download is `read_range` called in a
//! loop and written out as it arrives; an upload is forwarded to the daemon in
//! pieces. This head is a translator, not a buffer, and an attachment that
//! fits in memory today is not a reason to write code that assumes it always
//! will.
//!
//! **The response is decided before the body starts.** Once bytes are on the
//! wire the status line is spent, so a missing chunk found halfway through a
//! download would have to be reported by truncating — which is
//! indistinguishable from a network failure. Residency is resolved first.
//!
//! **User content is never rendered.** Every response is an attachment with
//! `nosniff` and a sandboxing CSP, whatever the stored MIME type says. This
//! server has one origin and it holds the session credential; a stored HTML
//! attachment rendered inline would run there.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::control::{ContentCall, ContentErrorCode, ContentReply};

use super::{err_json, socket, App, ErrorKind};

/// How often an in-flight upload samples its own progress.
///
/// The lane already coalesces and is lossy, so this is not a second throttle —
/// it is how often the producer bothers to build a frame at all. A 64 KiB piece
/// arrives far faster than twice a second, and every unsampled one would cost
/// two allocations to be discarded downstream.
const PROGRESS_SAMPLE: std::time::Duration = std::time::Duration::from_millis(500);

/// An operation id as a browser-stable key.
fn hex(bytes: &[u8; 16]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// How many content transfers this server will run at once, and how many of
/// those may be uploads.
///
/// Small, and acquired without waiting. A browser opens at most six connections
/// per origin over HTTP/1.1, shared with the event stream and every ordinary
/// request — so a transfer that queues does not queue politely, it starves the
/// rest of the page. Refusing with a `Retry-After` tells the client something it
/// can act on; blocking tells it nothing and looks like a hang.
const MAX_CONCURRENT_TRANSFERS: usize = 4;
const MAX_CONCURRENT_UPLOADS: usize = 2;

/// How long a client is told to wait after a refusal. One second: the transfers
/// this bounds are seconds long, not minutes.
const RETRY_AFTER_SECONDS: &str = "1";

/// The most plaintext one range request may ask for.
///
/// Matched to the engine's own `MAX_RANGE_BYTES` rather than chosen
/// independently, because a browser asking for more would get a short read and
/// have to guess why.
const MAX_RANGE_BYTES: u64 = runtime::plane::freight::content::MAX_RANGE_BYTES as u64;

/// The permits every content route acquires before it moves a byte.
///
/// On `App` rather than per-request state because the ceiling is a property of
/// this server, not of a handler.
pub struct ContentStreamPermits {
    transfers: Arc<tokio::sync::Semaphore>,
    uploads: Arc<tokio::sync::Semaphore>,
}

impl ContentStreamPermits {
    pub fn new() -> Self {
        Self {
            transfers: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TRANSFERS)),
            uploads: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_UPLOADS)),
        }
    }
}

/// Acquire without waiting, or refuse.
///
/// `try_acquire` and never `acquire`: a queued request holds one of the
/// browser's six connections while it waits, so waiting is how a slow transfer
/// becomes a hung page.
///
/// Owned permits, not borrowed ones. A download outlives the handler that
/// started it — the body is fed by a task — so a permit tied to the handler's
/// stack would be released the moment the response headers went out, and the
/// ceiling would bound how many transfers *begin* rather than how many are
/// running. Which is a bound on nothing.
type Permits = (
    tokio::sync::OwnedSemaphorePermit,
    Option<tokio::sync::OwnedSemaphorePermit>,
);

fn admit(permits: &ContentStreamPermits, upload: bool) -> Result<Permits, Response> {
    let transfer = permits
        .transfers
        .clone()
        .try_acquire_owned()
        .map_err(|_| busy())?;
    let write = if upload {
        Some(
            permits
                .uploads
                .clone()
                .try_acquire_owned()
                .map_err(|_| busy())?,
        )
    } else {
        None
    };
    Ok((transfer, write))
}

fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, RETRY_AFTER_SECONDS)],
        err_json(
            "this server is already moving as many files as it will at once",
            ErrorKind::Error,
        ),
    )
        .into_response()
}

/// Translate a content refusal into a status a browser can act on.
///
/// Each arm is a different thing for the client to do, which is why the daemon's
/// vocabulary is typed rather than a message. `NotResident` is the interesting
/// one: the content is real and this Station does not have the bytes yet, which
/// is a conflict to be resolved by fetching, not a 404 that would have the
/// browser cache the absence.
fn refuse_content(code: ContentErrorCode, message: &str) -> Response {
    let (status, kind) = match code {
        ContentErrorCode::Denied => (StatusCode::FORBIDDEN, ErrorKind::Error),
        ContentErrorCode::Unknown => (StatusCode::NOT_FOUND, ErrorKind::NotFound),
        ContentErrorCode::NotResident => (StatusCode::CONFLICT, ErrorKind::Error),
        ContentErrorCode::Sealed => (StatusCode::GONE, ErrorKind::Error),
        ContentErrorCode::Bounds => (StatusCode::RANGE_NOT_SATISFIABLE, ErrorKind::Error),
        ContentErrorCode::Storage => (StatusCode::SERVICE_UNAVAILABLE, ErrorKind::Error),
        ContentErrorCode::Invalid => (StatusCode::UNPROCESSABLE_ENTITY, ErrorKind::Error),
    };
    (status, err_json(message, kind)).into_response()
}

/// Whatever went wrong reaching the daemon, said once.
fn unreachable(error: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        err_json(error, ErrorKind::Error),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct RangeQuery {
    offset: Option<u64>,
    len: Option<u64>,
    /// The name to save as. Advisory: it is sanitised here regardless, and the
    /// client cannot use it to choose a path.
    name: Option<String>,
}

#[derive(Deserialize)]
pub struct UploadQuery {
    /// How many bytes the body carries. Required, and authoritative — see
    /// `docs/PROTOCOL.md` §10.1 for why a declared length is the only thing
    /// that distinguishes a truncated upload from a complete one.
    len: u64,
}

/// `HEAD /api/spaces/{id}/content/{content}` — geometry and residency.
///
/// The call a client makes before deciding whether a download is a download or
/// a transfer. Answered in headers rather than a body so it is a real HEAD.
pub(super) async fn head(
    State(app): State<Arc<App>>,
    Path((id, content)): Path<(String, String)>,
) -> Response {
    let Some(home) = station(&app, &id, false) else {
        return not_found(&id);
    };
    let home = match home {
        Ok(home) => home,
        Err(response) => return response,
    };
    let route = crate::control::station_route(home.address);
    match crate::control::content_call(
        home.daemon_home.as_path(),
        &crate::control::content_request(route, ContentCall::Stat { content }),
    )
    .await
    {
        Ok((
            ContentReply::ContentStatus {
                plaintext_len,
                chunk_count,
                resident_chunks,
                pinned,
                ..
            },
            _,
        )) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_LENGTH, number(plaintext_len));
            headers.insert("x-lait-chunk-count", number(chunk_count as u64));
            headers.insert("x-lait-resident-chunks", number(resident_chunks as u64));
            headers.insert(
                "x-lait-pinned",
                HeaderValue::from_static(if pinned { "1" } else { "0" }),
            );
            harden(&mut headers);
            (StatusCode::OK, headers).into_response()
        }
        Ok((ContentReply::ContentError { code, message }, _)) => refuse_content(code, &message),
        Ok((other, _)) => refuse_content(
            ContentErrorCode::Storage,
            &format!("unexpected answer: {other:?}"),
        ),
        Err(error) => unreachable(&format!("{error:#}")),
    }
}

/// `GET /api/spaces/{id}/content/{content}` — the file, or a range of it.
///
/// A bare GET answers the **whole** resource, streamed. It used to answer the
/// first 4 MiB with a 200 and a matching `Content-Length`, which is HTTP's only
/// way of saying "this is the entire thing" — so a browser saving a 30 MB video
/// wrote 4 MiB of it, with no error, no warning, and no way to notice: the body
/// was exactly as long as the header promised. The client that `Content-Disposition`
/// exists for is precisely the one that cannot loop.
///
/// A `Range:` header gets `206` and a `Content-Range`, which is what a browser
/// and every download manager already know how to resume. `Accept-Ranges` is
/// advertised so they know to try.
///
/// The bytes are still never accumulated here: the response body is fed by a
/// task looping `read_range`, so this process holds one range at a time
/// whatever the file's size.
pub(super) async fn download(
    State(app): State<Arc<App>>,
    Path((id, content)): Path<(String, String)>,
    Query(range): Query<RangeQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(home) = station(&app, &id, false) else {
        return not_found(&id);
    };
    let home = match home {
        Ok(home) => home,
        Err(response) => return response,
    };
    let Ok((transfer, _)) = admit(&app.content_permits, false) else {
        return busy();
    };

    // How large is it? Asked first, because every answer below needs it — a
    // `Content-Length` for the whole resource, the end of an open-ended range,
    // and the `Content-Range` total.
    let route = crate::control::station_route(home.address.clone());
    let total = match crate::control::content_call(
        home.daemon_home.as_path(),
        &crate::control::content_request(
            route.clone(),
            ContentCall::Stat {
                content: content.clone(),
            },
        ),
    )
    .await
    {
        Ok((ContentReply::ContentStatus { plaintext_len, .. }, _)) => plaintext_len,
        Ok((ContentReply::ContentError { code, message }, _)) => {
            return refuse_content(code, &message)
        }
        Ok((other, _)) => {
            return refuse_content(
                ContentErrorCode::Storage,
                &format!("unexpected answer: {other:?}"),
            )
        }
        Err(error) => return unreachable(&format!("{error:#}")),
    };

    // `?offset=`/`?len=` remain for a caller that wants one piece and knows it
    // — the control-plane shape. A `Range:` header wins when both are present,
    // because it is the one with agreed semantics.
    let requested = parse_range(headers.get(header::RANGE), total).or_else(|| {
        range.offset.or(range.len).map(|_| {
            (
                range.offset.unwrap_or(0),
                range.len.unwrap_or(MAX_RANGE_BYTES),
            )
        })
    });
    let partial = requested.is_some();
    let (start, wanted) = requested.unwrap_or((0, total));
    if start > total {
        return refuse_content(
            ContentErrorCode::Bounds,
            &format!("offset {start} is past the end of a {total}-byte content"),
        );
    }
    let length = wanted.min(total.saturating_sub(start));

    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_LENGTH, number(length));
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        disposition(range.name.as_deref()),
    );
    if partial {
        let last = start + length.saturating_sub(1);
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{last}/{total}"))
                .unwrap_or(HeaderValue::from_static("bytes */*")),
        );
    }
    harden(&mut response_headers);

    // One range in flight at a time, fed to the body as it arrives. The permit
    // rides the task rather than this function, because the transfer outlives
    // the handler that started it.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(2);
    let home_path = home.daemon_home.clone();
    tokio::spawn(async move {
        let _transfer = transfer;
        let mut at = start;
        let mut left = length;
        while left > 0 {
            let want = left.min(MAX_RANGE_BYTES);
            let answer = crate::control::content_call(
                home_path.as_path(),
                &crate::control::content_request(
                    route.clone(),
                    ContentCall::Read {
                        content: content.clone(),
                        offset: at,
                        len: want,
                    },
                ),
            )
            .await;
            let piece = match answer {
                Ok((ContentReply::ContentStream { .. }, bytes)) if !bytes.is_empty() => bytes,
                // Anything else mid-stream is a truncated file, and the only
                // honest report is to end the body short — the status line was
                // spent before the first byte.
                _ => {
                    let _ = tx
                        .send(Err(std::io::Error::other("the content ended early")))
                        .await;
                    return;
                }
            };
            at += piece.len() as u64;
            left = left.saturating_sub(piece.len() as u64);
            if tx.send(Ok(piece.into())).await.is_err() {
                return;
            }
        }
    });

    let status = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    (
        status,
        response_headers,
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
    )
        .into_response()
}

/// Parse a single-range `Range: bytes=…` header.
///
/// One range only. Multi-range replies are a multipart body, and a surface
/// whose whole job is handing a file to a browser has no use for one — a client
/// that asks for several gets the whole resource instead, which is what the
/// specification permits and what every browser copes with.
fn parse_range(value: Option<&HeaderValue>, total: u64) -> Option<(u64, u64)> {
    let raw = value?.to_str().ok()?;
    let spec = raw.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    if first.is_empty() {
        // `bytes=-N` — the final N bytes.
        let suffix: u64 = last.parse().ok()?;
        let suffix = suffix.min(total);
        return Some((total - suffix, suffix));
    }
    let start: u64 = first.parse().ok()?;
    let length = if last.is_empty() {
        total.saturating_sub(start)
    } else {
        let end: u64 = last.parse().ok()?;
        end.saturating_sub(start).saturating_add(1)
    };
    Some((start, length))
}

/// `POST /api/spaces/{id}/content?len=N` — a streaming upload.
///
/// The declared length is checked against nothing here on purpose: the Station
/// owns `max_content_len`, and duplicating its ceiling in this process would
/// give an operator two numbers to keep in step. What this does enforce is that
/// the body matches its declaration, because that is a property of *this*
/// request rather than of the Station's policy.
pub(super) async fn upload(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> Response {
    let Some(home) = station(&app, &id, true) else {
        return not_found(&id);
    };
    let home = match home {
        Ok(home) => home,
        Err(response) => return response,
    };
    let Ok((_transfer, _upload)) = admit(&app.content_permits, true) else {
        return busy();
    };

    let mut operation = [0u8; 16];
    if getrandom::fill(&mut operation).is_err() {
        return unreachable("could not mint an operation id");
    }
    let route = crate::control::station_route(home.address);
    let mut upload = match crate::control::ContentUpload::open(
        home.daemon_home.as_path(),
        route,
        operation,
        None,
        query.len,
    )
    .await
    {
        Ok(upload) => upload,
        Err(error) => return unreachable(&format!("{error:#}")),
    };

    // Forwarded as it arrives. A slow client holds one permit and one daemon
    // connection and nothing else — in particular it never holds its own upload
    // in this process's memory, which is the whole reason this route exists
    // rather than an inline JSON field.
    // Sampled here rather than emitted per piece. The lane coalesces and is
    // lossy by design, so a frame per 64 KiB would be discarded downstream
    // after costing two allocations to build.
    let transfer = hex(&operation);
    let mut moved = 0u64;
    let mut sampled = std::time::Instant::now();
    app.socket.note(socket::TransferProgress {
        transfer: transfer.clone(),
        content: String::new(),
        moved,
        total: query.len,
        done: false,
    });

    let mut stream = body.into_data_stream();
    loop {
        match next_piece(&mut stream).await {
            Ok(Some(piece)) => {
                if let Err(error) = upload.push(&piece).await {
                    return refuse_content(ContentErrorCode::Invalid, &format!("{error:#}"));
                }
                moved = moved.saturating_add(piece.len() as u64);
                if sampled.elapsed() >= PROGRESS_SAMPLE {
                    sampled = std::time::Instant::now();
                    app.socket.note(socket::TransferProgress {
                        transfer: transfer.clone(),
                        // An ingest mints its name at the end, so there is
                        // nothing truthful to put here until `finish`.
                        content: String::new(),
                        moved,
                        total: query.len,
                        done: false,
                    });
                }
            }
            Ok(None) => break,
            Err(error) => return refuse_content(ContentErrorCode::Invalid, &error),
        }
    }

    match upload.finish().await {
        Ok(ContentReply::ContentWritten {
            content,
            plaintext_len,
        }) => {
            app.socket.note(socket::TransferProgress {
                transfer,
                content: content.clone(),
                moved: plaintext_len,
                total: plaintext_len,
                done: true,
            });
            (
                StatusCode::CREATED,
                axum::Json(serde_json::json!({
                    "kind": "content",
                    "content": content,
                    "size": plaintext_len,
                })),
            )
                .into_response()
        }
        Ok(ContentReply::ContentError { code, message }) => refuse_content(code, &message),
        Ok(other) => refuse_content(
            ContentErrorCode::Storage,
            &format!("unexpected answer: {other:?}"),
        ),
        Err(error) => refuse_content(ContentErrorCode::Invalid, &format!("{error:#}")),
    }
}

async fn next_piece(
    stream: &mut axum::body::BodyDataStream,
) -> Result<Option<axum::body::Bytes>, String> {
    use tokio_stream::StreamExt as _;
    match stream.next().await {
        Some(Ok(piece)) => Ok(Some(piece)),
        Some(Err(error)) => Err(format!("the upload body ended early: {error}")),
        None => Ok(None),
    }
}

/// Where a content call for this space goes, and whether this identity may
/// write there.
struct ContentStation {
    address: crate::control::OrbitAddress,
    daemon_home: std::path::PathBuf,
}

fn station(app: &App, id: &str, write: bool) -> Option<Result<ContentStation, Response>> {
    let resolved = app.directory.resolve(id).ok()?;
    // A hosted identity's space is readable here and never writable, for the
    // same reason an ordinary write is refused: an upload through this surface
    // would be sealed and signed as that identity, by a person who is not it.
    if write {
        if let Some(refusal) = super::borrowed_key_refusal(&app.directory, &resolved, "an upload") {
            return Some(Err(refusal));
        }
    }
    Some(Ok(ContentStation {
        address: resolved.address,
        daemon_home: app.daemon.home().to_path_buf(),
    }))
}

fn not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        err_json(&format!("no such space: {id}"), ErrorKind::NotFound),
    )
        .into_response()
}

fn number(value: u64) -> HeaderValue {
    HeaderValue::from_str(&value.to_string()).unwrap_or(HeaderValue::from_static("0"))
}

/// The headers every content response carries, whatever it is.
///
/// One origin serves the viewer, the API, and this. A stored attachment
/// rendered inline would run in that origin, holding that session — so nothing
/// here is ever rendered: always a download, never sniffed, and sandboxed even
/// if some future arrangement did render it. The MIME type is deliberately not
/// the stored one; a peer chose that.
fn harden(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
}

/// `Content-Disposition` for a name a peer chose.
///
/// Two escapes, and they are not the same one. The name is first reduced to a
/// single safe file name by the shared sanitiser — which strips separators,
/// control characters and device names — and then percent-encoded into the
/// `filename*` form, because a header is a line and anything that could end it
/// early would inject a second one.
fn disposition(name: Option<&str>) -> HeaderValue {
    let safe = world_interface::destination::sanitize_display_name(name.unwrap_or("attachment"));
    let mut encoded = String::with_capacity(safe.len());
    for byte in safe.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{encoded}"))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_chosen_name_cannot_end_the_header_it_travels_in() {
        // A header is a line. A name carrying CR or LF would close this one and
        // start another of the attacker's choosing — and the sanitiser drops
        // control characters, so this is belt and braces, which is what a header
        // that carries remote data deserves.
        let value = disposition(Some("report.pdf\r\nX-Evil: yes"));
        let rendered = value.to_str().expect("a header value is ASCII");
        assert!(
            !rendered.contains('\r') && !rendered.contains('\n'),
            "{rendered}"
        );
        assert!(rendered.starts_with("attachment; filename*=UTF-8''"));
    }

    #[test]
    fn a_traversal_name_is_a_file_name_by_the_time_it_is_a_header() {
        let rendered = disposition(Some("../../evil.txt"))
            .to_str()
            .expect("ascii")
            .to_string();
        assert!(!rendered.contains(".."), "{rendered}");
        assert!(!rendered.contains('/'), "{rendered}");
    }

    #[test]
    fn an_absent_name_still_produces_a_saveable_one() {
        let rendered = disposition(None).to_str().expect("ascii").to_string();
        assert!(rendered.contains("attachment"), "{rendered}");
    }

    #[test]
    fn every_refusal_maps_to_something_the_client_can_act_on() {
        // The point of a typed refusal is that each one is a different next
        // move. A missing chunk is a conflict to resolve by fetching; an unknown
        // content never will be, and must not be cached as an absence.
        let cases = [
            (ContentErrorCode::Denied, StatusCode::FORBIDDEN),
            (ContentErrorCode::Unknown, StatusCode::NOT_FOUND),
            (ContentErrorCode::NotResident, StatusCode::CONFLICT),
            (ContentErrorCode::Bounds, StatusCode::RANGE_NOT_SATISFIABLE),
            (ContentErrorCode::Storage, StatusCode::SERVICE_UNAVAILABLE),
            (ContentErrorCode::Invalid, StatusCode::UNPROCESSABLE_ENTITY),
        ];
        for (code, expected) in cases {
            assert_eq!(
                refuse_content(code, "because").status(),
                expected,
                "{code:?}"
            );
        }
    }

    #[test]
    fn a_content_response_is_never_rendered() {
        let mut headers = HeaderMap::new();
        harden(&mut headers);
        assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("sandbox"));
    }

    #[tokio::test]
    async fn the_upload_lane_is_narrower_than_the_transfer_lane() {
        // Uploads cost the Station a writer lock and a disk; downloads cost a
        // read. Two ceilings because they are two different costs.
        let permits = ContentStreamPermits::new();
        let mut held = Vec::new();
        for _ in 0..MAX_CONCURRENT_UPLOADS {
            held.push(admit(&permits, true).map_err(|_| ()).expect("admitted"));
        }
        assert!(admit(&permits, true).is_err(), "an upload past the ceiling");
        assert!(
            admit(&permits, false).is_ok(),
            "a download is not blocked by uploads it does not share a ceiling with"
        );
    }
}

/// The whole path, through the router that serves it.
///
/// Everything above tests a decision in isolation. This tests the one thing
/// none of them can: that the route, the gate, the daemon envelope and a real
/// Station are wired to each other in that order, and that bytes come back the
/// same as they went in.
#[cfg(test)]
mod end_to_end {
    use super::*;
    use crate::daemon::{Client, LocalOrbitId};
    use crate::orbits::Catalog;
    use crate::serve::auth::Guard;
    use crate::serve::{cookie_name, router, App};
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    const TOKEN: &str = "1f2e3d4c5b6a798807162534435261700f1e2d3c4b5a69788796a5b4c3d2e1f0";
    const FOUNDER_SEED: [u8; 32] = [173u8; 32];

    struct MemFactory(comms::mem::MemNet);

    #[async_trait::async_trait]
    impl comms::TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &comms::policy::Network,
            _protocols: comms::Protocols<'_>,
        ) -> anyhow::Result<Arc<dyn comms::Transport>> {
            Ok(Arc::new(
                self.0
                    .peer(mechanics::actor::device_from_seed(identity_seed)),
            ))
        }
    }

    fn filler(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    fn authorized(method: &str, uri: &str, body: Body) -> HttpRequest<Body> {
        HttpRequest::builder()
            .uri(uri)
            .method(method)
            .header("host", "127.0.0.1:7717")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(body)
            .expect("request")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_goes_up_and_comes_back_the_same_file() {
        let dir = std::env::temp_dir().join(format!("lait-serve-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::orbital::form_space(&dir, &FOUNDER_SEED, "Serve Content").unwrap();
        let space = crate::orbital::discover_space(&dir).single().unwrap();

        let net = comms::mem::MemNet::new();
        let station_home = dir.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let _ = crate::orbital::run_station_process_with(
                    station_home,
                    FOUNDER_SEED,
                    &MemFactory(net),
                )
                .await;
            });
        });
        for _ in 0..200 {
            if matches!(
                crate::control::request(&dir, &crate::control::Request::Status).await,
                Ok(crate::control::Response::Status(_))
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let orbit = LocalOrbitId::for_store(&dir);
        let entry = crate::orbits::Entry {
            space: space.as_str().to_string(),
            name: "Serve Content".into(),
            path: dir.to_string_lossy().into_owned(),
            origin: crate::orbits::Origin::default(),
            host_nick: String::new(),
            last_opened: 0,
        };
        let app = Arc::new(App {
            world: crate::composition::PRODUCT_WORLD_MOUNT.to_owned(),
            head: crate::serve::head::Source::embedded(),
            selection: crate::config::Selection::default(),
            guard: Guard::new(TOKEN.into(), 7717),
            directory: Catalog::with_entries(dir.clone(), dir.clone(), true, vec![entry]),
            daemon: Client::at(dir.clone()),
            doorbells: tokio::sync::broadcast::channel(4).0,
            cookie: cookie_name(7717),
            launch_tickets: crate::serve::auth::LaunchTickets::new(),
            stop: tokio::sync::watch::channel(false).0,
            content_permits: ContentStreamPermits::new(),
            socket: crate::serve::socket::Hub::new(),
        });

        // Attached before the upload starts, because the lane is live and not
        // a log: a watcher that subscribes afterwards has missed it.
        let mut progress = app.socket.subscribe();

        // Larger than one control-channel read buffer, so the body's first bytes
        // are the ones that land inside the header line's buffer — the mistake a
        // small upload cannot catch.
        let plaintext = filler(21, 700 * 1024);
        let response = router(app.clone())
            .oneshot(authorized(
                "POST",
                &format!("/api/spaces/{orbit}/content?len={}", plaintext.len()),
                Body::from(plaintext.clone()),
            ))
            .await
            .expect("upload");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "upload refused: {}",
            String::from_utf8_lossy(&body)
        );
        let written: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let content = written["content"]
            .as_str()
            .expect("a content id")
            .to_string();
        assert_eq!(written["size"].as_u64(), Some(plaintext.len() as u64));

        // The progress lane had no producer at all, so a browser subscribing to
        // it watched a transfer it was never told about.
        let mut frames = Vec::new();
        while let Ok(frame) = progress.try_recv() {
            frames.push(frame);
        }
        let opened = frames.first().expect("an upload announces that it began");
        assert!(!opened.done);
        assert_eq!(opened.total, plaintext.len() as u64);
        assert!(
            opened.content.is_empty(),
            "an ingest has no name until it finishes"
        );
        let finished = frames.last().expect("and announces that it finished");
        assert!(finished.done);
        assert_eq!(finished.content, content, "named once there is a name");
        assert_eq!(finished.moved, plaintext.len() as u64);
        assert_eq!(
            finished.transfer, opened.transfer,
            "one transfer, keyed the same throughout"
        );

        // HEAD answers geometry and residency in headers, and we sealed it, so
        // every chunk is here.
        let head = router(app.clone())
            .oneshot(authorized(
                "HEAD",
                &format!("/api/spaces/{orbit}/content/{content}"),
                Body::empty(),
            ))
            .await
            .expect("head");
        assert_eq!(head.status(), StatusCode::OK);
        let headers = head.headers().clone();
        assert_eq!(headers["content-length"], plaintext.len().to_string());
        assert_eq!(
            headers["x-lait-resident-chunks"], headers["x-lait-chunk-count"],
            "we sealed it, so we hold all of it"
        );
        assert_eq!(headers["content-type"], "application/octet-stream");
        assert_eq!(headers["x-content-type-options"], "nosniff");

        // And it comes back in ranges, byte for byte.
        let mut got: Vec<u8> = Vec::new();
        while got.len() < plaintext.len() {
            let response = router(app.clone())
                .oneshot(authorized(
                    "GET",
                    &format!(
                        "/api/spaces/{orbit}/content/{content}?offset={}&len=262144&name=notes.txt",
                        got.len()
                    ),
                    Body::empty(),
                ))
                .await
                .expect("download");
            // An explicit offset/len is a partial request, and says so.
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            let disposition = response.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .to_string();
            assert!(disposition.contains("notes.txt"), "{disposition}");
            let piece = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .expect("body");
            assert!(!piece.is_empty(), "a short read that never ends is a hang");
            got.extend_from_slice(&piece);
        }
        assert_eq!(
            blake3::hash(&got),
            blake3::hash(&plaintext),
            "the round trip lost or reordered bytes"
        );

        // A bare GET is the whole file, not the first slice of it.
        //
        // This used to answer 200 with a Content-Length of 4 MiB — HTTP's only
        // way of saying "this is all of it" — so a browser saving a large file
        // wrote a truncated one with no error and no way to notice, because the
        // body was exactly as long as the header promised. The client that
        // Content-Disposition exists for is the one that cannot loop.
        let whole = router(app.clone())
            .oneshot(authorized(
                "GET",
                &format!("/api/spaces/{orbit}/content/{content}?name=notes.txt"),
                Body::empty(),
            ))
            .await
            .expect("download");
        assert_eq!(whole.status(), StatusCode::OK);
        assert_eq!(
            whole.headers()[header::CONTENT_LENGTH],
            plaintext.len().to_string(),
            "a bare GET must declare the whole length"
        );
        assert_eq!(whole.headers()[header::ACCEPT_RANGES], "bytes");
        let body = axum::body::to_bytes(whole.into_body(), 8 << 20)
            .await
            .expect("body");
        assert_eq!(
            blake3::hash(&body),
            blake3::hash(&plaintext),
            "a bare GET returned a truncated file"
        );

        // And a Range gets 206 with a Content-Range, which is what a browser
        // and every download manager already know how to resume.
        let ranged = router(app.clone())
            .oneshot({
                let mut request = authorized(
                    "GET",
                    &format!("/api/spaces/{orbit}/content/{content}"),
                    Body::empty(),
                );
                request
                    .headers_mut()
                    .insert(header::RANGE, "bytes=100-199".parse().unwrap());
                request
            })
            .await
            .expect("ranged download");
        assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            ranged.headers()[header::CONTENT_RANGE],
            format!("bytes 100-199/{}", plaintext.len())
        );
        let piece = axum::body::to_bytes(ranged.into_body(), 1 << 20)
            .await
            .expect("body");
        assert_eq!(&piece[..], &plaintext[100..200]);

        // A content id nobody has heard of is a 404, and it says nothing about
        // whether it exists anywhere else.
        let unknown = router(app.clone())
            .oneshot(authorized(
                "HEAD",
                &format!("/api/spaces/{orbit}/content/{}", "ab".repeat(32)),
                Body::empty(),
            ))
            .await
            .expect("head");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let _ = crate::control::request(&dir, &crate::control::Request::Stop).await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
