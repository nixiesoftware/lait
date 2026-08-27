//! The served World document.
//!
//! There is deliberately no compiled-in product floor here. A page is read
//! only from the selected immutable World release, so removing or updating a
//! World changes one independently owned installation rather than revealing a
//! second copy hidden in the host executable.
//!
//! Serving it from the daemon — rather than from a dev server or a CDN — is also
//! what makes the client **same-origin**, which is the precondition for the
//! `Origin` allowlist in [`super::auth`] meaning anything at all.
//!
//! The document is served exactly as the World wrote it. There used to be a
//! client overlay composed into every HTML response on the way out — presence,
//! transfer progress, and a route back to the client — with a nonce, a closed
//! capability list and a trust line dividing what could be shown in the page
//! from what had to raise the client instead. It is gone, and with it the only
//! reason `asset` and `index` were ever more than byte pipes.

use std::borrow::Cow;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// The document type. Everything else is served as the bytes it is.
///
/// Named rather than repeated, because "is this a document" stopped being a
/// header value and became a decision: [`compose`] rewrites what this labels and
/// must not touch anything else.
const HTML: &str = "text/html; charset=utf-8";

/// Content types for what a vite build actually emits.
///
/// Hand-rolled rather than pulling `mime_guess`: this is a closed set we produce
/// ourselves, not arbitrary user files. The default is deliberately
/// `application/octet-stream` — an unknown asset should download inertly rather
/// than be sniffed and executed as something we didn't intend.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => HTML,
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Serve one asset by path, or the SPA entry when nothing matches.
///
/// The fallback is what makes client-side routing work: an unknown path is a
/// route for the app to resolve, not a 404 — the app is the only thing that knows
/// its own routes. Paths that escape the release simply miss and fall back to
/// that release's entry document.
///
/// Documents go through [`compose`]; everything else is handed back as the exact
/// bytes the release carries.
pub fn asset(path: &str, head: &crate::serve::head::Source) -> Response {
    let path = path.trim_start_matches('/');
    if let Some(bytes) = head.read(path) {
        let mime = content_type(path);
        let body: Cow<'static, [u8]> = Cow::Owned(bytes);
        return ([(header::CONTENT_TYPE, mime)], body).into_response();
    }
    index(head)
}

/// The SPA entry, from the activated bundle when it carries one.
pub fn index(head: &crate::serve::head::Source) -> Response {
    if let Some(bytes) = head.read("index.html") {
        let body: Cow<'static, [u8]> = Cow::Owned(bytes);
        return ([(header::CONTENT_TYPE, HTML)], body).into_response();
    }
    // A refused link is a different fact from an empty release, and saying the
    // wrong one costs an afternoon: the release named here is usually fine, and
    // the mistake is a directory that does not exist. The `tracing::error` the
    // refusal already emits cannot be relied on to carry it — a head installs a
    // subscriber only when it was started as a daemon — so the reason travels
    // to the one place somebody is certainly looking.
    if let Some(why) = head.refusal() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{}=… names a directory this head refuses to serve: {why}\n\
                 \n\
                 The installed release was not consulted: a link that falls back \
                 would answer a question nobody asked, and a typo would look \
                 exactly like an edit that did nothing.",
                crate::serve::head::LINK_VAR
            ),
        )
            .into_response();
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the selected World release carries no web entry document".to_owned(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_cover_what_vite_emits() {
        assert_eq!(content_type("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("index.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        // Unknown extensions must not be guessed into something executable.
        assert_eq!(content_type("weird.xyz"), "application/octet-stream");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }

    async fn body_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .expect("read the refusal body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The failure this seam exists to stop producing, arriving by the door it
    /// left open: a typo in the link is not a broken release, and saying so is
    /// the whole difference between a two-second fix and an afternoon spent
    /// reinstalling a World that was never at fault.
    #[tokio::test]
    async fn a_refused_link_says_so_instead_of_blaming_the_release() {
        let source =
            crate::serve::head::Source::refused("/no/such/directory is not a directory".to_owned());
        let response = index(&source);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = body_of(response).await;
        assert!(
            body.contains(crate::serve::head::LINK_VAR),
            "the refusal must name the variable that caused it: {body}"
        );
        assert!(
            body.contains("/no/such/directory"),
            "the refusal must carry the path that was refused: {body}"
        );
        assert!(
            !body.contains("release carries no web entry"),
            "a refused link must not be reported as an empty release: {body}"
        );
    }

    /// The ordinary absence keeps the ordinary sentence. Both are 503 and only
    /// the reason differs, so this is what stops the fix above from turning
    /// every empty release into a story about an environment variable nobody
    /// set.
    #[tokio::test]
    async fn an_empty_release_is_still_reported_as_an_empty_release() {
        let response = index(&crate::serve::head::Source::unavailable());
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = body_of(response).await;
        assert!(body.contains("carries no web entry document"), "{body}");
        assert!(
            !body.contains(crate::serve::head::LINK_VAR),
            "nothing was linked, so nothing should mention linking: {body}"
        );
    }
}
