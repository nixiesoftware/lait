//! The served client — the React app compiled into the binary, and the client
//! overlay the head composes over the documents it serves.
//!
//! `include_dir!` reads `src/serve/assets` at **compile time**, which is why that
//! directory holds *build output* and is *committed*. Three facts force it:
//! `Cargo.toml` excludes `viewer/` from the published crate, `publish-crates.yml`
//! is Rust-only, and `build.rs` deliberately never shells out to git so a plain
//! `cargo install lait` stays reproducible with no external toolchain. Building
//! the bundle during `cargo build` would need npm; leaving it in `viewer/` would
//! mean crates.io users get a head with no UI. So it lives here, in git.
//!
//! The honest cost is build output under version control, kept fresh by
//! `npm run build` (which writes straight here) and guarded by CI diffing a
//! rebuild. See `docs/UI.md`, web surface.
//!
//! Serving it from the daemon — rather than from a dev server or a CDN — is also
//! what makes the client **same-origin**, which is the precondition for the
//! `Origin` allowlist in [`super::auth`] meaning anything at all.
//!
//! # The overlay, and why the head is the one that draws it
//!
//! A World ships its own head and draws its own pages, and none of them knows
//! there is a client around it. The overlay is that missing context — who else
//! is here, what this device is moving, the way back to the client's window.
//!
//! The head is the only party positioned to add it, because the head already
//! answers for the page. Nothing here needs browser automation, an extension, or
//! a client that owns the window, and none of it changes if `Open` later hands
//! the page to a client-owned frame instead of the person's browser.
//!
//! So [`asset`] and [`index`] stop being byte pipes for one kind of file: an
//! HTML **document** is composed on the way out, and everything else — script,
//! stylesheet, font, image — is still the embedded bytes, unread and unchanged.
//! Four properties hold that seam together, each of them a failure that would
//! otherwise be invisible until a World's page broke in the field:
//!
//! - **Only documents are composed.** A `<div>` spliced into a `.js` is a syntax
//!   error at the top of the application, and into a `.woff2` is a font the
//!   browser refuses.
//! - **Composition is deterministic and idempotent.** The same document composes
//!   to the same bytes every time it is served, and a document that already
//!   carries the marker is left exactly as it is — otherwise a reverse proxy, a
//!   cache, or a second pass would stack overlays.
//! - **A document with no recognisable injection point is served untouched.**
//!   [`body_open_end`] answers `None` for everything it is not certain about. A
//!   malformed injection breaks a World's page; a missing overlay leaves it
//!   precisely as the World wrote it, which is the failure worth having.
//! - **Convenience, never authority.** See [`Authority`].
//!
//! ## What the marker actually guarantees
//!
//! Every composed document carries `data-lait-overlay="<nonce>"`, where the
//! nonce is 32 bytes of system entropy minted once per process
//! ([`overlay_nonce`]). Being precise about what that buys, because it is easy
//! to claim more:
//!
//! - **It defeats a pre-baked forgery.** A World authors its bytes before this
//!   process starts, so nothing it ships can carry this run's marker. Markup
//!   that imitates the overlay is therefore distinguishable from the overlay in
//!   the bytes the head served, and it is distinguishable at parse time — the
//!   injection is in the served document, so at first paint, before a single
//!   line of the World's script has run, the only nonce-bearing container in the
//!   document is the head's.
//! - **It does not defeat a scripted forgery.** The overlay lives in the DOM of
//!   the World's own page, same origin, so the World's script can read the nonce
//!   back out of it, mint a look-alike carrying the same value, or simply cover
//!   or delete the real one. Same-origin DOM has no privilege boundary in it and
//!   no marker can invent one.
//!
//! The two alternatives that would close that gap were considered and are not
//! available at this layer. A sandboxed `srcdoc` frame gets an opaque origin the
//! World cannot read into — but an opaque origin also cannot reach `/api`, so
//! the overlay could never learn presence or progress except from the World,
//! which is worse than what it fixes. Serving the overlay from a *second*
//! loopback origin does close it properly, and it is a change to the listener
//! and the `Origin` allowlist rather than to composition — it belongs to
//! whoever binds the port, not here.
//!
//! Which is exactly why the trust line below is drawn in the code and not in a
//! comment: an overlay a World can imitate must not be an overlay a World can
//! spend anything through.

use std::borrow::Cow;
use std::sync::OnceLock;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use include_dir::{include_dir, Dir};

static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/serve/assets");

/// The one content type composition keys off.
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
/// its own routes. Paths that escape the bundle simply miss and fall back too;
/// `include_dir` resolves against an embedded tree, not the filesystem, so there
/// is no directory to traverse out of.
///
/// Documents go through [`compose`]; everything else is handed back as the exact
/// bytes that were embedded.
pub fn asset(path: &str, overlay: bool) -> Response {
    let path = path.trim_start_matches('/');
    if let Some(file) = ASSETS.get_file(path) {
        let mime = content_type(path);
        let body = if mime == HTML && overlay {
            compose(file.contents())
        } else {
            Cow::Borrowed(file.contents())
        };
        return ([(header::CONTENT_TYPE, mime)], body).into_response();
    }
    index(overlay)
}

/// The SPA entry.
pub fn index(overlay: bool) -> Response {
    match ASSETS.get_file("index.html") {
        Some(f) => {
            let body = if overlay {
                compose(f.contents())
            } else {
                Cow::Borrowed(f.contents())
            };
            ([(header::CONTENT_TYPE, HTML)], body).into_response()
        }
        // Only reachable if someone ships a build with an empty assets dir.
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "lait was built without its web client (src/serve/assets is empty — run `npm run build` in viewer/)",
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// The trust line
// ---------------------------------------------------------------------------

/// Which side of the trust line a capability falls on.
///
/// Not a label: [`render`] reads it, and it decides what markup a capability is
/// permitted to produce at all. That is the difference between a rule and a
/// comment about a rule — deleting the classification does not weaken a policy
/// somewhere else, it stops the markup from being generated.
///
/// The line is where it is because the overlay lives in the DOM of a page a
/// World serves. A World can draw over it and imitate it, so the worst a lie
/// can cost has to be bounded: a wrong belief about who is present is a wrong
/// belief, and a forged *Accept* would be a membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// Shows a fact. Rendered inline, over the World's page. Being lied to here
    /// costs a wrong belief and never a grant.
    Convenience,
    /// Grants, admits, approves, or spends — or is the plain route back to the
    /// client. Cannot happen in the page at all: it renders as one link *out* of
    /// the document, and the client is where it completes.
    RaisesTheClient,
}

/// Everything the overlay carries.
///
/// Closed and enumerated on purpose. A capability that arrived as loose markup
/// would arrive without anybody deciding which side of [`Authority`] it falls
/// on, and the default it inherited would be the permissive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Who else is in this World right now.
    Presence,
    /// How far the transfers this device is running have got.
    TransferProgress,
    /// The route back to the client's own window.
    OpenTheClient,
    /// Invite somebody into this Space — an admission, so not from here.
    InviteSomeone,
    /// Take an invitation this device has been offered — an admission of this
    /// device, so not from here.
    AcceptInvitation,
    /// Admit another of this person's devices — a key ceremony, so not from
    /// here, and not from anything that can be drawn over.
    ApproveDevice,
}

impl Capability {
    /// Every capability, in the order the overlay draws them.
    ///
    /// The order is the render order, so composition stays deterministic without
    /// anything having to sort.
    pub const ALL: &'static [Self] = &[
        Self::Presence,
        Self::TransferProgress,
        Self::OpenTheClient,
        Self::InviteSomeone,
        Self::AcceptInvitation,
        Self::ApproveDevice,
    ];

    /// Which side of the trust line this act falls on.
    pub const fn authority(self) -> Authority {
        match self {
            Self::Presence | Self::TransferProgress => Authority::Convenience,
            Self::OpenTheClient
            | Self::InviteSomeone
            | Self::AcceptInvitation
            | Self::ApproveDevice => Authority::RaisesTheClient,
        }
    }

    /// The machine-readable name: the `data-` attribute, and the last segment of
    /// the raise URL. Stable, because a client registering the scheme handler
    /// matches on it.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Presence => "presence",
            Self::TransferProgress => "transfers",
            Self::OpenTheClient => "open",
            Self::InviteSomeone => "invite",
            Self::AcceptInvitation => "accept-invitation",
            Self::ApproveDevice => "approve-device",
        }
    }

    /// What a person reads. The ellipsis on the authority acts is the honest
    /// promise: pressing this opens something else, it does not do the thing.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Presence => "Here",
            Self::TransferProgress => "Transfers",
            Self::OpenTheClient => "Open lait",
            Self::InviteSomeone => "Invite…",
            Self::AcceptInvitation => "Accept an invitation…",
            Self::ApproveDevice => "Approve a device…",
        }
    }
}

/// Where a raise travels.
///
/// `lait://` is already this project's scheme — an invite is
/// `lait://join/<ticket>`, parsed by `runtime::coordinates` — so a raise is a
/// sibling namespace under it rather than a second scheme the OS has to learn.
///
/// Until a client registers the handler, following one of these does nothing at
/// all: the browser has nowhere to send it. That is the correct failure. An
/// authority act that quietly fell back to completing in the page would be the
/// whole point of this module, undone.
const RAISE_BASE: &str = "lait://client/";

/// The attribute that marks a composed document, and carries this run's nonce.
///
/// Used for three things at once: the idempotence check reads it, the overlay's
/// container carries it, and a test asserts nothing in the embedded bundle can.
/// Deliberately absent from the stylesheet below — the CSS hooks on a class
/// instead, so *counting* this string counts overlays and not selectors.
const OVERLAY_MARKER: &str = "data-lait-overlay";

/// The overlay's own presentation, namespaced so it cannot reach a World's page.
///
/// A fixed chip in the corner rather than a bar across the top: the overlay is
/// drawn over somebody else's layout and must not take the space that layout was
/// designed around, and must not sit over anything a person clicks. `color-scheme`
/// is set explicitly because the World's page decides the document's, and an
/// overlay that inverted with it would be unreadable half the time.
const OVERLAY_STYLE: &str = concat!(
    ".lait-overlay{position:fixed;right:12px;bottom:12px;z-index:2147483647;display:flex;",
    "gap:8px;align-items:center;padding:6px 8px;border:1px solid rgba(255,255,255,.14);",
    "border-radius:8px;background:rgba(20,20,23,.86);color:#e9e9ec;color-scheme:dark;",
    "font:12px/1.4 ui-sans-serif,system-ui,sans-serif;box-shadow:0 4px 16px rgba(0,0,0,.4)}",
    ".lait-overlay .lait-fact{display:flex;gap:4px;align-items:baseline;white-space:nowrap}",
    ".lait-overlay .lait-name{opacity:.62}",
    ".lait-overlay .lait-value{font-variant-numeric:tabular-nums}",
    ".lait-overlay .lait-raise{color:inherit;text-decoration:none;white-space:nowrap;",
    "padding:2px 6px;border:1px solid rgba(255,255,255,.18);border-radius:6px}",
    ".lait-overlay .lait-raise:hover{background:rgba(255,255,255,.08)}",
);

/// Render one capability as the only markup its side of the line permits.
///
/// This is where [`Authority`] stops being a word. A convenience capability
/// becomes a fact: a name, a value, and nothing that can be pressed. An
/// authority capability becomes exactly one link, out of the document, to the
/// client — no button, no form, no handler, and (see [`overlay`]) no script
/// anywhere in the overlay for one to be attached to. The page therefore holds
/// no path along which such an act could complete, which is a stronger statement
/// than "we did not write one", because it survives a World's script driving the
/// overlay's own elements.
///
/// The convenience value is an em dash and not a zero. Nothing has sampled
/// presence yet — the live lanes are `/api/session`'s, and wiring them is not
/// this seam — and a zero would be a figure nobody measured.
fn render(capability: Capability) -> String {
    let slug = capability.slug();
    let label = capability.label();
    match capability.authority() {
        Authority::Convenience => format!(
            "<span class=\"lait-fact\" data-lait-fact=\"{slug}\">\
             <span class=\"lait-name\">{label}</span>\
             <span class=\"lait-value\">\u{2014}</span></span>"
        ),
        // `target="_top"` because a World may have framed this document, and a
        // raise that resolved inside a frame the World controls is a raise the
        // World gets to intercept.
        Authority::RaisesTheClient => format!(
            "<a class=\"lait-raise\" data-lait-raise=\"{slug}\" href=\"{base}{slug}\" \
             target=\"_top\" rel=\"noopener\">{label}</a>",
            base = RAISE_BASE,
        ),
    }
}

/// The whole injected fragment for one run.
///
/// Contains no caller-supplied text — every string in it is a literal from this
/// module or the hex nonce — so there is no escaping question to get wrong, and
/// no path by which a World's data could reach the markup the head composes.
///
/// It also contains no script. That is a choice, not an omission: a script would
/// be an in-page code path, and the raise links do not need one. A plain anchor
/// navigating out is the *browser's* behaviour rather than ours, which is the
/// only kind of "leaves the page" that a page cannot re-enter.
fn overlay(nonce: &str) -> String {
    let rendered: String = Capability::ALL.iter().copied().map(render).collect();
    format!(
        "<div class=\"lait-overlay\" {marker}=\"{nonce}\" role=\"complementary\" \
         aria-label=\"lait client\"><style>{OVERLAY_STYLE}</style>{rendered}</div>",
        marker = OVERLAY_MARKER,
    )
}

/// The marker this run stamps on every document it composes.
///
/// One value per process, minted from system entropy on first use and never
/// persisted — the same shape and the same strength as the run token, for the
/// same reason: a value a World could predict is a value a World could pre-bake.
///
/// `None` when entropy was unavailable, and then nothing is composed at all. An
/// overlay carrying a guessable marker would be worse than no overlay: it would
/// put a surface on the page that a World could reproduce exactly, while looking
/// to a person like the one the head vouches for.
fn overlay_nonce() -> Option<&'static str> {
    static NONCE: OnceLock<Option<String>> = OnceLock::new();
    NONCE
        .get_or_init(|| match super::auth::mint_token() {
            Ok(nonce) => Some(nonce),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "no entropy for an overlay marker; serving documents uncomposed"
                );
                None
            }
        })
        .as_deref()
}

/// Compose the overlay into one HTML document.
///
/// Every refusal below hands the document back **borrowed and unchanged**, and
/// they are all the same judgement: composition happens only where it is certain
/// to be correct. A World whose page arrives without an overlay has lost some
/// context; a World whose page arrives with markup spliced into the middle of an
/// attribute value has lost the page.
fn compose(document: &[u8]) -> Cow<'_, [u8]> {
    match overlay_nonce() {
        Some(nonce) => compose_with(document, nonce),
        None => Cow::Borrowed(document),
    }
}

/// [`compose`], with the nonce supplied.
///
/// Split out so determinism and idempotence are testable as themselves: with the
/// process-wide nonce baked in, "the same document composes to the same bytes"
/// could only be checked against whatever this run happened to mint.
fn compose_with<'a>(document: &'a [u8], nonce: &str) -> Cow<'a, [u8]> {
    // A document that is not UTF-8 is not one this scanner can reason about, and
    // guessing at an encoding to inject into is how you corrupt one.
    let Ok(html) = std::str::from_utf8(document) else {
        return Cow::Borrowed(document);
    };
    // Idempotence. The marker is what a second pass sees — through a proxy, a
    // cache, or simply this function called twice — and stacking overlays is the
    // failure it prevents.
    if html.contains(OVERLAY_MARKER) {
        return Cow::Borrowed(document);
    }
    let Some(at) = body_open_end(html) else {
        return Cow::Borrowed(document);
    };
    let (Some(head), Some(tail)) = (html.get(..at), html.get(at..)) else {
        return Cow::Borrowed(document);
    };
    Cow::Owned(format!("{head}{}{tail}", overlay(nonce)).into_bytes())
}

/// Elements whose content is raw text rather than markup.
///
/// A `<body` inside one of these is a string in a script or a word in a title,
/// not the document's body — and injecting there would put a `<div>` inside a
/// JavaScript string literal.
const RAW_TEXT: [&str; 4] = ["script", "style", "textarea", "title"];

/// The offset just past the document's `<body>` start tag, or `None`.
///
/// Deliberately a scanner over the document's prefix and not a parse. It walks
/// from the start, steps over the two constructs that can carry a literal
/// `<body` which is not a tag — an HTML comment and [`RAW_TEXT`] content — and
/// reads attributes quote-aware so a `>` inside an attribute value cannot be
/// mistaken for the end of a tag.
///
/// Everything it is not sure about is `None`: an unterminated comment, a raw-text
/// element that never closes, a `<` with no `>` after it. Those are documents
/// whose structure this cannot establish, and the caller's answer to that is to
/// serve them exactly as they arrived.
fn body_open_end(html: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut at = 0usize;
    while let Some(&byte) = bytes.get(at) {
        if byte != b'<' {
            at = at.saturating_add(1);
            continue;
        }
        // `<` is ASCII, so `at` is a char boundary and this cannot split one.
        let rest = html.get(at..)?;
        if rest.starts_with("<!--") {
            // Unterminated: the rest of the document is commentary, and there is
            // no body in it to find.
            let end = rest.find("-->")?;
            at = at.saturating_add(end).saturating_add(3);
            continue;
        }
        let end = tag_end(html, at)?;
        match tag_name(rest) {
            Some(name) if name.eq_ignore_ascii_case("body") => return Some(end),
            Some(name) if RAW_TEXT.iter().any(|raw| name.eq_ignore_ascii_case(raw)) => {
                at = tag_end(html, find_close_tag(html, end, name)?)?;
            }
            // An end tag, a doctype, a comment-like declaration, or any other
            // element: skip the whole tag rather than one byte of it, so a `<`
            // inside its attributes is never rescanned as a tag of its own.
            _ => at = end,
        }
    }
    None
}

/// The element name of the start tag at the front of `rest`, borrowed exactly as
/// the document spells it — callers compare it case-insensitively, because HTML
/// does. `None` for anything that is not a start tag: an end tag, a doctype, a
/// stray `<` in text.
fn tag_name(rest: &str) -> Option<&str> {
    let after = rest.strip_prefix('<')?;
    let len = after
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric())
        .count();
    after.get(..len).filter(|name| !name.is_empty())
}

/// Where the tag beginning at `from` ends: the offset just past its `>`.
///
/// Quote-aware, which is the entire reason it exists. `<body class="a>b">` ends
/// at the *second* `>`, and a scanner that took the first would splice the
/// overlay into the middle of an attribute value — a page that renders as a
/// broken tag and a stray attribute, which is exactly the "malformed injection
/// is worse than none" case.
fn tag_end(html: &str, from: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut quote: Option<u8> = None;
    let mut at = from;
    while let Some(&byte) = bytes.get(at) {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return at.checked_add(1),
            None => {}
        }
        at = at.saturating_add(1);
    }
    None
}

/// The offset of `</name` at or after `from`, ignoring case — `</SCRIPT>` closes
/// a `<script>`, and a scanner that missed it would treat the whole rest of the
/// document as script text and find no body at all.
fn find_close_tag(html: &str, from: usize, name: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let needle = format!("</{name}");
    let needle = needle.as_bytes();
    let last = bytes.len().checked_sub(needle.len())?;
    (from..=last).find(|&start| {
        bytes
            .get(start..start.saturating_add(needle.len()))
            .is_some_and(|window| window.eq_ignore_ascii_case(needle))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_is_actually_embedded() {
        // The failure this catches is a build that silently ships no UI — the
        // whole point of committing the bundle.
        assert!(
            ASSETS.get_file("index.html").is_some(),
            "index.html missing"
        );
        assert!(ASSETS.get_file("app.js").is_some(), "app.js missing");
    }

    #[test]
    fn content_types_cover_what_vite_emits() {
        assert_eq!(content_type("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("index.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        // Unknown extensions must not be guessed into something executable.
        assert_eq!(content_type("weird.xyz"), "application/octet-stream");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }
}

/// The composition seam: what gets rewritten, what does not, and where the
/// trust line falls.
#[cfg(test)]
mod composition {
    use super::*;

    /// A fixed nonce, so "the same document composes to the same bytes" is a
    /// statement about composition and not about what this run happened to mint.
    const NONCE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn composed(document: &str) -> String {
        String::from_utf8(compose_with(document.as_bytes(), NONCE).into_owned())
            .expect("composition produced invalid UTF-8")
    }

    /// Whether `haystack` contains `needle` anywhere in it, byte for byte.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    async fn body_of(response: Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .expect("read the response body")
            .to_vec()
    }

    #[tokio::test]
    async fn an_asset_passes_through_byte_identical() {
        // The failure: composition that keys off "did this come from the bundle"
        // rather than off the content type. A `<div>` in a `.js` is a syntax
        // error at the top of the application and in a `.woff2` is a font the
        // browser refuses — both of them silent until something renders.
        for path in ["app.js", "index.css", "inter-latin-wght-normal.woff2"] {
            let embedded = ASSETS.get_file(path).expect(path).contents();
            let served = body_of(asset(path, true)).await;
            assert_eq!(
                served,
                embedded,
                "{path} was rewritten on the way out ({} bytes vs {})",
                served.len(),
                embedded.len(),
            );
        }
    }

    /// The overlay is *client context*. A head somebody opened themselves has
    /// none to draw, and an overlay offering a route back to a client that is
    /// not running is a control that cannot work — worse than absent, because
    /// it looks like a feature.
    #[tokio::test]
    async fn a_head_nobody_launched_from_the_client_serves_no_overlay() {
        for served in [
            body_of(index(false)).await,
            body_of(asset("/index.html", false)).await,
        ] {
            assert!(
                !contains(&served, OVERLAY_MARKER.as_bytes()),
                "a head with no client behind it drew a client overlay",
            );
        }

        // And what it serves is the embedded document, byte for byte — the
        // ungated path is not a second, subtly different composition.
        let embedded = ASSETS.get_file("index.html").expect("index").contents();
        assert_eq!(body_of(index(false)).await, embedded);
    }

    #[tokio::test]
    async fn the_documents_the_head_serves_are_composed() {
        // Both doors: the SPA entry and the same file reached as an asset. They
        // are separate functions, and a seam added to one of them is a seam a
        // person finds by opening the app the other way.
        for served in [
            body_of(index(true)).await,
            body_of(asset("/index.html", true)).await,
        ] {
            assert!(
                contains(&served, OVERLAY_MARKER.as_bytes()),
                "a document was served without the overlay",
            );
        }
    }

    #[test]
    fn composing_twice_produces_the_same_bytes_and_a_composed_document_is_left_alone() {
        const PAGE: &str = "<!doctype html><html><body><p>a World's page</p></body></html>";

        let once = compose_with(PAGE.as_bytes(), NONCE).into_owned();
        let twice = compose_with(PAGE.as_bytes(), NONCE).into_owned();
        assert_eq!(once, twice, "composition is not deterministic");

        // The stacking failure: a second pass — a proxy, a cache, this function
        // called twice — must not add a second overlay.
        let again = compose_with(&once, NONCE);
        assert!(
            matches!(again, Cow::Borrowed(_)),
            "an already-composed document was composed again",
        );
        assert_eq!(again.as_ref(), once.as_slice());
    }

    #[test]
    fn a_document_with_no_recognisable_injection_point_is_served_unchanged() {
        // Every one of these is a document whose structure the scanner cannot
        // establish. Serving it untouched is the answer: the overlay is worth
        // less than the page it would be spliced into.
        for document in [
            "<!doctype html><html><head></head></html>", // no body at all
            "<p>a fragment, not a document</p>",
            "<!-- <body> --><p>only ever in a comment</p>",
            "<html><body", // a start tag that never ends
            "<!-- unterminated <body>",
            "<html><head><script>var s = \"<body>\";</head></html>", // script never closes
            "",
        ] {
            assert!(
                matches!(compose_with(document.as_bytes(), NONCE), Cow::Borrowed(_)),
                "composed a document it should have left alone: {document}",
            );
        }
    }

    #[test]
    fn a_document_that_is_not_utf8_is_served_unchanged() {
        let document = [b'<', b'b', b'o', b'd', b'y', b'>', 0xff, 0xfe];
        assert!(matches!(compose_with(&document, NONCE), Cow::Borrowed(_)));
    }

    #[test]
    fn an_attribute_containing_a_bracket_does_not_end_the_body_tag() {
        // Found by reasoning about what a `>` means inside quotes, which is the
        // one thing a naïve `find('>')` gets wrong — and it gets it wrong by
        // emitting a page with a broken tag rather than by failing.
        let page = composed("<html><body class=\"a>b\" data-x='c>d'><p>x</p></body></html>");
        assert!(
            page.contains("<body class=\"a>b\" data-x='c>d'><div class=\"lait-overlay\""),
            "spliced into an attribute value: {page}",
        );
    }

    #[test]
    fn a_body_written_inside_a_comment_or_a_script_is_not_the_injection_point() {
        let page = composed(concat!(
            "<html><head><!-- <body> -->",
            "<script>var s = \"<body>\";</script>",
            "<style>/* <body> */</style>",
            "</head><body id=\"real\"><p>x</p></body></html>",
        ));
        assert!(
            page.contains("<body id=\"real\"><div class=\"lait-overlay\""),
            "injected at a `<body` that was text, not a tag: {page}",
        );
        assert_eq!(
            page.matches(OVERLAY_MARKER).count(),
            1,
            "more than one overlay in one document: {page}",
        );
    }

    /// Half the trust line: the overlay really is drawn, in the page, over a
    /// World that never implemented any of it.
    #[test]
    fn a_convenience_surface_renders_over_a_worlds_page() {
        let page = composed(
            "<!doctype html><html><body><h1>a World that never heard of a client</h1></body></html>",
        );

        // The World's document is intact and its content still comes first.
        assert!(page.contains("<h1>a World that never heard of a client</h1>"));

        for capability in Capability::ALL {
            if capability.authority() != Authority::Convenience {
                continue;
            }
            let slug = capability.slug();
            assert!(
                page.contains(&format!("data-lait-fact=\"{slug}\"")),
                "{slug} is not drawn in the page: {page}",
            );
            // A convenience surface never carries the machinery of a raise —
            // otherwise the classification would be decorative.
            assert!(
                !page.contains(&format!("data-lait-raise=\"{slug}\"")),
                "{slug} rendered as a raise",
            );
        }

        // And nothing is synthesised. Nothing has sampled presence, so the value
        // is absent rather than zero; a zero is a figure, and reporting one
        // nobody measured is the defect class the client's own rules name.
        assert!(
            page.contains("<span class=\"lait-value\">\u{2014}</span>"),
            "an unmeasured figure was rendered as a number: {page}",
        );
    }

    /// The other half, and the point of the issue: nothing that grants can
    /// finish here.
    ///
    /// A World can draw over the overlay and imitate it, so the only durable
    /// defence is that an authority act has no in-page path to complete on. The
    /// assertions are structural rather than about a handler's behaviour,
    /// because a handler that behaves correctly today is a handler somebody can
    /// edit — whereas markup that contains no script, no form and no endpoint
    /// has nothing to edit.
    #[test]
    fn an_authority_action_leaves_the_page() {
        let page = composed("<!doctype html><html><body><p>a World's page</p></body></html>");

        for capability in Capability::ALL {
            if capability.authority() != Authority::RaisesTheClient {
                continue;
            }
            let slug = capability.slug();
            assert!(
                page.contains(&format!(
                    "data-lait-raise=\"{slug}\" href=\"lait://client/{slug}\""
                )),
                "{slug} is not a link out to the client: {page}",
            );
            // It must not also be drawn as something that acts here.
            assert!(
                !page.contains(&format!("data-lait-fact=\"{slug}\"")),
                "{slug} rendered as an in-page surface",
            );
        }

        // And the overlay carries no machinery for one to complete on at all.
        // The World's page in this test contains none of these strings, so every
        // hit would be the overlay's.
        for machinery in ["<script", "fetch(", "/api/", "<form", "<button", "onclick"] {
            assert!(
                !page.contains(machinery),
                "the overlay carries `{machinery}`; an authority act could complete in the page",
            );
        }
    }

    /// The classification itself, spelled out by name.
    ///
    /// Derived from `authority()` it would be a tautology. Written out, adding a
    /// capability fails here until somebody decides which side of the line it
    /// falls on — which is the decision that is easy to skip and expensive to
    /// skip.
    #[test]
    fn every_act_that_grants_admits_or_approves_is_on_the_far_side_of_the_line() {
        assert_eq!(Capability::Presence.authority(), Authority::Convenience);
        assert_eq!(
            Capability::TransferProgress.authority(),
            Authority::Convenience
        );
        assert_eq!(
            Capability::OpenTheClient.authority(),
            Authority::RaisesTheClient
        );
        assert_eq!(
            Capability::InviteSomeone.authority(),
            Authority::RaisesTheClient
        );
        assert_eq!(
            Capability::AcceptInvitation.authority(),
            Authority::RaisesTheClient
        );
        assert_eq!(
            Capability::ApproveDevice.authority(),
            Authority::RaisesTheClient
        );
        assert_eq!(
            Capability::ALL.len(),
            6,
            "a capability was added without a row here deciding which side it is on",
        );
    }

    /// What the marker buys, and — in the doc, because the code cannot assert a
    /// negative about a browser — what it does not.
    ///
    /// It buys this: a World authors its bytes before this process exists, so
    /// nothing it ships can carry this run's marker, and at parse time the only
    /// nonce-bearing container in the document is the head's.
    ///
    /// It does not buy freedom from a scripted forgery. The overlay is in the
    /// World's own document, same origin, so the World's script can read the
    /// nonce back out and mint a look-alike — which is precisely why
    /// [`an_authority_action_leaves_the_page`] is the test that matters.
    #[test]
    fn the_marker_is_minted_per_run_and_nothing_a_world_authored_can_carry_it() {
        let nonce = overlay_nonce().expect("this machine has entropy");
        assert_eq!(nonce.len(), 64, "the marker is a 32-byte credential in hex");
        assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));

        // Per run: the next mint is a different value, so a marker observed once
        // is worth nothing to the next process.
        let next = super::super::auth::mint_token().expect("mint");
        assert_ne!(nonce, next, "the marker is deterministic across runs");

        // And it is stable *within* a run — the whole idempotence story depends
        // on the same document composing to the same bytes.
        assert_eq!(Some(nonce), overlay_nonce());

        // Nothing the bundle shipped carries it, because the bundle was written
        // before this process started.
        for path in ["index.html", "app.js"] {
            let authored = ASSETS.get_file(path).expect(path).contents();
            assert!(
                !contains(authored, nonce.as_bytes()),
                "{path} carries this run's marker",
            );
        }
    }
}
