//! The browser-facing seam for Signage.
//!
//! Signage shipped able to be *rendered* — a display surface a receiver
//! consumes — and unable to be *driven*: the package declared no web parser,
//! so `POST /api/spaces/{id}/worlds/signage/rpc` answered every request with
//! "World 'signage' does not expose a web client interface". A World that only
//! a screen can reach has no authoring surface at all.
//!
//! There is very little here on purpose. [`SignageRequest`] is already the
//! product's own strict schema, tagged `cmd` and rendered in snake case, so
//! the browser's wire shape *is* that enum and this module is a decoder rather
//! than a second protocol. Adding a request variant reaches the browser with
//! no edit here beyond [`COMMANDS`], which is the property worth having: two
//! hand-written translations of one schema is how the head and the daemon come
//! to disagree about what a request means.

use runtime::world::call::Access;
use serde_json::Value;
use world_interface::{
    ClientAccess, ClientHost, ClientInvocation, Failure, HostContentRequest, LocalInvocation,
};

use crate::protocol::SignageRequest;

/// The `cmd` values this build serves, named in a refusal so a caller learns
/// what was available rather than only that it was wrong.
///
/// Pinned against the request enum by test, because a list that lags the enum
/// misleads exactly the caller who is already lost.
const COMMANDS: &str = "program_get, program_list, program_put, program_delete, \
     media_get, media_list, media_put, media_delete, media_used_by, \
     screen_get, screen_list, screen_put, screen_delete, screen_showing, screen_plays, \
     group_get, group_list, group_put, group_delete, \
     config_get, config_list, config_put, config_delete";

/// The one local operation this package owns.
///
/// Ingest cannot be a World command, because the derivation has to read the
/// file and a World may not read plaintext by design. It cannot be the caller's
/// job either, because the refusal is the point: a file that cannot meet the
/// plane's baseline has no valid catalog, and the person uploading it is the
/// one who can do something about that — telling them at a render instead
/// reaches a screen at three in the morning.
pub const LOCAL_MEDIA_INGEST: &str = "signage.media_ingest";

/// One local invocation: derive, seal, record.
pub fn execute<'a>(
    host: &'a dyn ClientHost,
    local: LocalInvocation,
) -> world_interface::ClientFuture<'a, Value> {
    Box::pin(async move {
        if local.operation != LOCAL_MEDIA_INGEST {
            return Err(Failure::new(format!(
                "unsupported Signage local operation '{}'",
                local.operation
            )));
        }
        let path = local
            .input
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::new("media_ingest requires a 'file' path"))?
            .to_string();
        let name = local
            .input
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                std::path::Path::new(&path)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone())
            });
        run_media_ingest(host, path, name, local.input).await
    })
}

/// Derive the catalog, seal the bytes, write the library entry.
///
/// The order is the contract. Derivation runs first because it is the step
/// that can refuse, and a refusal must cost nothing: no content is sealed, no
/// record written, the person is told and the Space never hears about it. The
/// content is committed second, and the entry that names it third — the
/// substrate refuses a declaration whose descriptor is not committed, so this
/// order cannot race, it can only fail cleanly.
async fn run_media_ingest(
    host: &dyn ClientHost,
    path: String,
    name: String,
    input: Value,
) -> Result<Value, Failure> {
    // 1. Derive, from the local file, before anything is spent. `find_moov`
    //    walks box headers, so this reads the table of contents and the sample
    //    tables — not the film.
    let file = std::fs::File::open(&path)
        .map_err(|error| Failure::new(format!("could not open {path}: {error}")))?;
    let total = file
        .metadata()
        .map_err(|error| Failure::new(format!("could not stat {path}: {error}")))?
        .len();
    let policy = mediabox::CatalogPolicy {
        max_group_duration_ms: runtime::plane::live::media::DEFAULT_MAX_GROUP_DURATION_MS,
        target_latency_ms: runtime::plane::live::media::DEFAULT_MAX_LATENCY_MS,
        jitter_hint_ms: 50,
        // The rendition a receiver's ticket resolves against is the content's
        // own name once one exists; until the seal below there is none, so a
        // placeholder derives and the record's id is authoritative.
        rendition: "main".into(),
    };
    let media = {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        let read = |offset: u64, size: u32| {
            let mut bytes =
                vec![0u8; usize::try_from(size).map_err(|_| mediabox::Failure::Container)?];
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| mediabox::Failure::Container)?;
            let mut filled = 0usize;
            while let Some(window) = bytes.get_mut(filled..).filter(|window| !window.is_empty()) {
                match file.read(window) {
                    Ok(0) => break,
                    Ok(n) => filled = filled.saturating_add(n),
                    Err(_) => return Err(mediabox::Failure::Container),
                }
            }
            bytes.truncate(filled);
            Ok(bytes)
        };
        mediabox::read_catalog(total, read, &policy).map_err(|error| {
            Failure::new(format!("{name} cannot be served as signage media: {error}"))
        })?
    };
    let catalog = String::from_utf8(
        media
            .catalog
            .encode_canonical()
            .map_err(|_| Failure::new("the derived catalog would not encode"))?,
    )
    .map_err(|_| Failure::new("the derived catalog is not UTF-8"))?;
    let (width, height) = media
        .tracks
        .iter()
        .find_map(|(_, shape)| shape.width.zip(shape.height))
        .map(|(w, h)| (Some(w), Some(h)))
        .unwrap_or((None, None));

    // 2. Seal. The file is streamed by the shell, never read into this process
    //    a second time.
    let stored = host
        .call_content(HostContentRequest::Write {
            path: std::path::PathBuf::from(&path),
        })
        .await?;
    let content = stored
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("the content plane stored the file but did not name it"))?
        .to_string();
    let size = stored
        .get("size")
        .and_then(Value::as_u64)
        .unwrap_or_default();

    // 3. Record. The id is minted here, the way the web parser's `media_put`
    //    path mints one, so the caller supplies intent and never identity.
    let entry = signage::contract::SignageMedia {
        id: replica::body::BodyId::mint()
            .map_err(|error| Failure::new(format!("could not mint a media id: {error}")))?
            .render(),
        name,
        source: signage::contract::MediaSource::Stored {
            content,
            size,
            mime: input
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or("video/mp4")
                .to_string(),
        },
        duration_ms: None,
        width,
        height,
        catalog: Some(catalog),
    };
    let call = crate::encode_call(&SignageRequest::MediaPut {
        media: entry.clone(),
    })
    .map_err(|error| Failure::new(error.to_string()))?;
    let reply = host.call_world(call.clone()).await?;
    crate::decode_reply(&call, reply).map_err(|error| Failure::new(error.to_string()))?;
    Ok(serde_json::json!({
        "kind": "media",
        "media": entry.id,
        "tracks": entry.catalog.is_some(),
    }))
}

/// Construct one Signage World invocation with package-owned client policy.
///
/// The access class is read off the same [`SignageRequest::access`] that
/// `SignageCallHandler::access` runs on the daemon's side, so the head's copy
/// and the daemon's cannot describe the same bytes differently. This one is
/// *head* policy — what a client may draw and whether it must confirm; the
/// daemon derives its own classification after the call arrives and that is
/// what authorization consults.
pub fn world_invocation(request: SignageRequest) -> Result<ClientInvocation, Failure> {
    let confirmation = request.destructive_question();
    let access = match request.access() {
        Access::Query => ClientAccess::Query,
        Access::Command => ClientAccess::Command,
    };
    let call = crate::encode_call(&request).map_err(|error| Failure::new(error.to_string()))?;
    Ok(ClientInvocation::world(call, access, confirmation))
}

/// Decode the Signage browser protocol behind its explicit World route.
///
/// The two failures are kept apart because they are different mistakes: a body
/// with no `cmd` is not a Signage request at all, while a named command that
/// will not decode is a Signage request whose payload is wrong. Answering both
/// with one message would make a typo and a schema change look alike.
pub fn parse_web(input: Value) -> Result<ClientInvocation, Failure> {
    let command = input
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("Signage request is missing string field 'cmd'"))?
        .to_owned();
    match serde_json::from_value::<SignageRequest>(input) {
        Ok(request) => world_invocation(request),
        Err(error) => Err(Failure::new(format!(
            "Signage request '{command}' could not be read ({COMMANDS}): {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that records what was asked of it and answers like the shell.
    struct RecordingHost {
        content_writes: std::sync::Mutex<Vec<std::path::PathBuf>>,
        world_calls: std::sync::Mutex<Vec<runtime::world::call::Call>>,
    }

    impl RecordingHost {
        fn new() -> Self {
            Self {
                content_writes: std::sync::Mutex::new(Vec::new()),
                world_calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl world_interface::ClientHost for RecordingHost {
        fn local_root(&self) -> &std::path::Path {
            std::path::Path::new(".")
        }

        fn call_world<'a>(
            &'a self,
            call: runtime::world::call::Call,
        ) -> world_interface::ClientFuture<'a, runtime::world::call::Reply> {
            self.world_calls.lock().unwrap().push(call.clone());
            Box::pin(async move {
                let body = serde_json::to_vec(&serde_json::json!({"kind": "ok"})).unwrap();
                Ok(runtime::world::call::Reply::ok(&call, body))
            })
        }

        fn call_work<'a>(
            &'a self,
            _request: runtime::exec::WorkRequest,
        ) -> world_interface::ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::new("no work here")) })
        }

        fn call_control<'a>(
            &'a self,
            _request: world_interface::HostControlRequest,
        ) -> world_interface::ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::new("no control here")) })
        }

        fn call_content<'a>(
            &'a self,
            request: HostContentRequest,
        ) -> world_interface::ClientFuture<'a, Value> {
            let path = match &request {
                HostContentRequest::Write { path } => path.clone(),
                _ => return Box::pin(async { Err(Failure::new("only writes here")) }),
            };
            self.content_writes.lock().unwrap().push(path);
            Box::pin(async {
                Ok(serde_json::json!({
                    "content": "ab".repeat(32),
                    "size": 4_096,
                }))
            })
        }

        fn call_identity<'a>(
            &'a self,
            _handles: Vec<world_interface::PresentationHandle>,
        ) -> world_interface::ClientFuture<'a, world_interface::PresentationResolution> {
            Box::pin(async { Ok(world_interface::PresentationResolution::unavailable()) })
        }
    }

    /// Nothing in the mock host pends, so a noop waker is the whole executor —
    /// the same precedent issues-app's host tests set.
    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("the ingest seam must not pend against a mock"),
        }
    }

    /// The ingest seam: derive, seal, record — and refuse before spending.
    ///
    /// The refusal half is the point of the seam existing. A file the plane
    /// cannot package is told to the person uploading it, and costs nothing:
    /// no content sealed, no record written, the Space never hears of it.
    #[test]
    fn media_ingest_derives_before_it_seals_and_refuses_before_it_spends() {
        let dir = std::env::temp_dir().join(format!("signage-ingest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A real container, from the shared fixtures: ftyp, mdat, moov after.
        let film = dir.join("ribbon-cutting.mp4");
        std::fs::write(&film, mediabox::testkit::whole_file()).unwrap();

        let host = RecordingHost::new();
        let outcome = block_on(execute(
            &host,
            world_interface::LocalInvocation {
                operation: LOCAL_MEDIA_INGEST.into(),
                input: json!({"file": film.to_string_lossy(), "name": "Ribbon cutting"}),
            },
        ))
        .expect("a real container ingests");
        assert_eq!(outcome.get("kind").and_then(Value::as_str), Some("media"));

        // Derive ran first and the seal second: exactly one write, and one
        // MediaPut carrying what the derivation built.
        assert_eq!(host.content_writes.lock().unwrap().len(), 1);
        assert_eq!(host.world_calls.lock().unwrap().len(), 1);

        // A file the plane cannot package is refused with nothing spent.
        let noise = dir.join("noise.bin");
        std::fs::write(&noise, b"this is not a container at all").unwrap();
        let spent = RecordingHost::new();
        let refused = block_on(execute(
            &spent,
            world_interface::LocalInvocation {
                operation: LOCAL_MEDIA_INGEST.into(),
                input: json!({"file": noise.to_string_lossy()}),
            },
        ));
        assert!(refused.is_err(), "noise is refused where the person is");
        assert!(
            spent.content_writes.lock().unwrap().is_empty(),
            "a refusal costs nothing: no content sealed"
        );
        assert!(
            spent.world_calls.lock().unwrap().is_empty(),
            "a refusal costs nothing: no record written"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
    use crate::protocol::every_verb;
    use replica::body::BodyId;
    use serde_json::json;

    fn program_id() -> String {
        BodyId::from_bytes([7; 16]).render()
    }

    /// The wire form of a request, which is what a browser actually posts.
    fn wire(request: &SignageRequest) -> Value {
        serde_json::to_value(request).unwrap()
    }

    fn command_of(request: &SignageRequest) -> String {
        wire(request)
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned()
    }

    #[test]
    fn a_query_parses_and_stays_a_query() {
        let invocation = parse_web(json!({ "cmd": "program_list" })).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Query);
        assert_eq!(invocation.world_id(), &signage::contract::world_id());
        assert!(invocation.confirmation_question().is_none());
    }

    /// Every verb survives the round trip through the browser's wire form with
    /// the class and the question the protocol assigned it.
    #[test]
    fn every_verb_parses_back_with_the_class_the_protocol_assigns() {
        for (request, access) in every_verb() {
            let command = command_of(&request);
            let expected = match access {
                Access::Query => ClientAccess::Query,
                Access::Command => ClientAccess::Command,
            };
            let asks = request.destructive_question().is_some();
            let invocation = parse_web(wire(&request)).unwrap_or_else(|error| {
                panic!("{command} did not parse: {error}");
            });
            assert_eq!(invocation.access(), expected, "{command}");
            assert_eq!(
                invocation.confirmation_question().is_some(),
                asks,
                "{command}"
            );
        }
    }

    #[test]
    fn every_delete_is_a_command_and_asks_by_name() {
        let mut deletes = 0;
        for (request, _) in every_verb() {
            let Some(question) = request.destructive_question() else {
                continue;
            };
            deletes += 1;
            let wire = wire(&request);
            let target = wire
                .as_object()
                .unwrap()
                .iter()
                .find(|(field, _)| field.as_str() != "cmd")
                .and_then(|(_, value)| value.as_str())
                .unwrap()
                .to_owned();
            let invocation = parse_web(wire).unwrap();
            assert_eq!(invocation.access(), ClientAccess::Command);
            assert_eq!(
                invocation.confirmation_question(),
                Some(question.as_str()),
                "the head asks the protocol's question, not its own"
            );
            assert!(question.contains(&target), "got: {question}");
        }
        assert_eq!(deletes, 5, "one delete per document type");
    }

    #[test]
    fn a_media_put_is_a_command_that_does_not_ask() {
        let (request, _) = every_verb()
            .into_iter()
            .find(|(request, _)| matches!(request, SignageRequest::MediaPut { .. }))
            .expect("the verb table serves a media put");
        let invocation = parse_web(wire(&request)).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Command);
        assert!(invocation.confirmation_question().is_none());
    }

    /// The repairable detail, which is what a caller fixing its request needs.
    ///
    /// `Display` renders only the stable classification on purpose, so a test
    /// that reads it back cannot tell any two malformed requests apart.
    fn diagnostic(error: &Failure) -> String {
        error
            .diagnostic()
            .expect("an adapter refusal carries the detail that repairs it")
            .to_owned()
    }

    #[test]
    fn a_body_without_a_command_is_not_a_signage_request() {
        let error = parse_web(json!({ "program": program_id() })).unwrap_err();
        let message = diagnostic(&error);
        assert!(
            message.contains("missing string field 'cmd'"),
            "got: {message}"
        );
    }

    #[test]
    fn an_unknown_command_is_refused_by_name_and_lists_what_exists() {
        let error = parse_web(json!({ "cmd": "program_publish" })).unwrap_err();
        let message = diagnostic(&error);
        assert!(message.contains("program_publish"), "got: {message}");
        assert!(message.contains("program_list"), "got: {message}");
        assert!(message.contains("media_put"), "got: {message}");
        assert!(message.contains("screen_showing"), "got: {message}");
    }

    /// The listed commands are the served commands, both ways.
    ///
    /// A name here that no verb answers sends a caller to write a request that
    /// cannot work, and a verb missing from it is unreachable by anyone reading
    /// the refusal.
    #[test]
    fn the_refusal_lists_exactly_the_commands_this_build_serves() {
        let listed: std::collections::BTreeSet<&str> = COMMANDS.split(',').map(str::trim).collect();
        let served: std::collections::BTreeSet<String> = every_verb()
            .iter()
            .map(|(request, _)| command_of(request))
            .collect();
        let served: std::collections::BTreeSet<&str> = served.iter().map(String::as_str).collect();
        assert_eq!(listed, served);
    }

    #[test]
    fn a_known_command_with_a_wrong_payload_is_refused_as_a_payload_problem() {
        let error = parse_web(json!({ "cmd": "program_get" })).unwrap_err();
        let message = diagnostic(&error);
        assert!(message.contains("program_get"), "got: {message}");
        assert!(
            !message.contains("missing string field 'cmd'"),
            "a payload fault must not read as a missing command: {message}"
        );
    }

    #[test]
    fn the_package_now_exposes_a_web_client_interface() {
        let package = crate::package().unwrap();
        let invocation = package.parse_web(json!({ "cmd": "program_list" })).unwrap();
        assert_eq!(invocation.world_id(), &signage::contract::world_id());
    }
}
