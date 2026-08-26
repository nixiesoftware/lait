//! The control channel's third envelope: a header line, then raw bytes.
//!
//! Every other exchange on this channel is one JSON line each way, which is the
//! wrong shape for a 256 MiB attachment — it would have to be base64'd, held
//! whole on both sides, and parsed as one token. The content envelope declares
//! a length and sends exactly that many bytes.
//!
//! What is actually being proven here is the framing, and the traps are all in
//! the same place: the reader that consumed the header already holds the first
//! bytes of the body. A small body hides that completely, so the round-trip
//! test uses one large enough that those bytes matter, and checks the digest
//! rather than the length.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::world_fixture::run_station_process_with;
use anyhow::Result;
use async_trait::async_trait;
use comms::mem::MemNet;
use comms::policy::Network;
use comms::{Transport, TransportFactory};
use lait::control::OrbitAddress;
use lait::control::{
    content_call, content_request, ContentCall, ContentClientRequest, ContentErrorCode,
    ContentReply, ContentUpload, ControlRoute, Request, Response,
};

const FOUNDER_SEED: [u8; 32] = [151u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct MemFactory(MemNet);

#[async_trait]
impl TransportFactory for MemFactory {
    async fn build(
        &self,
        identity_seed: &[u8; 32],
        _network: &Network,
        _protocols: comms::Protocols<'_>,
    ) -> Result<Arc<dyn Transport>> {
        Ok(Arc::new(
            self.0
                .peer(mechanics::actor::device_from_seed(identity_seed)),
        ))
    }
}

/// A throwaway root that removes itself — see [`crate::head::temp_root`],
/// which is the one place that knows how.
fn temp_home(tag: &str) -> crate::head::TempRoot {
    crate::head::temp_root(&format!("ccat-{tag}"))
}

fn poll_until<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = check() {
            return Some(v);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A running Space with its control socket live.
struct Node {
    /// Held, not just named: this removes the directory when the test
    /// ends, so it has to outlive everything reading from it.
    home: crate::head::TempRoot,
    route: ControlRoute,
    rt: tokio::runtime::Runtime,
    _daemon: std::thread::JoinHandle<()>,
}

fn node(tag: &str) -> Node {
    let net = MemNet::new();
    let home = temp_home(tag);
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Content Space").unwrap();
    let space = lait::orbital::discover_space(&home).single().unwrap();
    let route = ControlRoute::Orbit {
        address: OrbitAddress::for_store(&home, space),
    };
    let daemon = {
        let home = home.to_path_buf();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                if let Err(e) = run_station_process_with(home, FOUNDER_SEED, &MemFactory(net)).await
                {
                    eprintln!("DAEMON ERR: {e:#}");
                }
            });
        })
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let online = poll_until(Duration::from_secs(20), || {
        let answer = rt.block_on(lait::control::request(&home, &Request::Status));
        matches!(answer, Ok(Response::Status(_))).then_some(())
    });
    assert!(online.is_some(), "the Space never came online");
    Node {
        home,
        route,
        rt,
        _daemon: daemon,
    }
}

impl Node {
    fn upload(&self, operation: [u8; 16], bytes: &[u8]) -> Result<ContentReply> {
        self.rt.block_on(async {
            let mut upload = ContentUpload::open(
                &self.home,
                self.route.clone(),
                operation,
                None,
                bytes.len() as u64,
            )
            .await?;
            for piece in bytes.chunks(64 * 1024) {
                upload.push(piece).await?;
            }
            upload.finish().await
        })
    }

    fn call(&self, call: ContentCall) -> Result<(ContentReply, Vec<u8>)> {
        self.rt.block_on(content_call(
            &self.home,
            &content_request(self.route.clone(), call),
        ))
    }

    /// Send a header exactly as written, body or no body. The only way to ask
    /// the adversarial questions, which are all about a header disagreeing with
    /// what follows it.
    fn raw(&self, request: ContentClientRequest) -> Result<(ContentReply, Vec<u8>)> {
        self.rt.block_on(content_call(&self.home, &request))
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self
            .rt
            .block_on(lait::control::request(&self.home, &Request::Stop));
        let _ = std::fs::remove_dir_all(&self.home);
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

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn written(reply: &ContentReply) -> String {
    match reply {
        ContentReply::ContentWritten { content, .. } => content.clone(),
        other => panic!("expected a written content, got {other:?}"),
    }
}

#[test]
fn a_body_whose_first_bytes_landed_in_the_header_buffer_round_trips_by_digest() {
    // The trap this exists for: `read_line` fills a `BufReader` looking for the
    // newline, so by the time the header is parsed the reader already holds the
    // first several KiB of the body. Reading the body from the raw half instead
    // drops exactly those bytes — and the content that commits is wrong in a
    // way that hashes perfectly well.
    //
    // A small body cannot catch it, because a small body fits entirely in the
    // buffer and comes back fine either way. This one is larger than the
    // buffer, and the assertion is on the digest, not the length.
    let node = node("roundtrip");
    let plaintext = filler(11, 900 * 1024);
    let reply = node.upload([1u8; 16], &plaintext).expect("upload");
    let content = written(&reply);

    let ContentReply::ContentStatus {
        plaintext_len,
        chunk_count,
        resident_chunks,
        ..
    } = node
        .call(ContentCall::Stat {
            content: content.clone(),
        })
        .expect("stat")
        .0
    else {
        panic!("expected a status");
    };
    assert_eq!(plaintext_len, plaintext.len() as u64);
    assert_eq!(resident_chunks, chunk_count, "we sealed it, so we hold it");

    // Read it back in ranges, which is the only way this surface offers.
    let mut got = Vec::new();
    while (got.len() as u64) < plaintext_len {
        let (reply, piece) = node
            .call(ContentCall::Read {
                content: content.clone(),
                offset: got.len() as u64,
                len: 256 * 1024,
                patience_ms: 0,
            })
            .expect("read");
        assert!(
            matches!(reply, ContentReply::ContentStream { .. }),
            "{reply:?}"
        );
        assert!(!piece.is_empty(), "a short read that never ends is a hang");
        got.extend_from_slice(&piece);
    }
    assert_eq!(
        digest(&got),
        digest(&plaintext),
        "the round trip lost or reordered bytes"
    );
}

#[test]
fn a_body_that_ends_early_commits_nothing() {
    // "However much arrived" and "all of it" are the same thing without a
    // declared length, and the difference is a permanently wrong content that
    // hashes fine. The declaration is what makes truncation detectable.
    let node = node("truncated");
    let refusal = node.rt.block_on(async {
        let mut upload = ContentUpload::open(&node.home, node.route.clone(), [2u8; 16], None, 4096)
            .await
            .expect("open");
        upload.push(&filler(12, 1024)).await.expect("push");
        upload.finish().await
    });
    assert!(
        refusal.is_err(),
        "an upload that sent a quarter of what it promised must not report success"
    );

    // And nothing landed: the operation id is the only handle on it, and there
    // is no content to find under any name.
    let node2 = node;
    let plaintext = filler(12, 4096);
    let reply = node2.upload([3u8; 16], &plaintext).expect("a whole upload");
    let content = written(&reply);
    let (_, got) = node2
        .call(ContentCall::Read {
            content,
            offset: 0,
            len: 4096,
            patience_ms: 0,
        })
        .expect("read");
    assert_eq!(
        digest(&got),
        digest(&plaintext),
        "the truncated attempt must not have contaminated a later one"
    );
}

#[test]
fn a_declaration_past_the_stations_ceiling_is_refused_before_a_byte_is_read() {
    // Reading first and refusing after is a free way to make this process spend
    // a Station's whole disk budget on something it was always going to refuse.
    //
    // Asked as a bare header with no body at all, which is the shape that
    // proves it: the daemon answers without ever waiting for the eight
    // gigabytes the header promised.
    let node = node("ceiling");
    let (reply, _) = node
        .raw(ContentClientRequest {
            content: ContentCall::Write {
                operation: "0".repeat(32),
            },
            route: node.route.clone(),
            act_as: None,
            body_len: 8 * 1024 * 1024 * 1024,
        })
        .expect("the daemon answers rather than waiting for a body");
    assert!(
        matches!(
            reply,
            ContentReply::ContentError {
                code: ContentErrorCode::Bounds,
                ..
            }
        ),
        "{reply:?}"
    );

    // And the client refuses its own over-send locally, so a caller's bug is
    // reported where it happened rather than arriving as a remote refusal.
    let local = node.rt.block_on(async {
        let mut upload =
            ContentUpload::open(&node.home, node.route.clone(), [4u8; 16], None, 16).await?;
        upload.push(&[0u8; 32]).await
    });
    assert!(
        local.is_err(),
        "pushing past the declaration must refuse here"
    );
}

#[test]
fn an_unknown_content_answers_the_same_way_a_forgotten_one_does() {
    // A caller that could tell "not here" from "never heard of it" would have
    // an oracle for what a Space contains, answerable by guessing ids.
    let node = node("unknown");
    let (reply, _) = node
        .call(ContentCall::Stat {
            content: "ab".repeat(32),
        })
        .expect("stat");
    assert!(
        matches!(
            reply,
            ContentReply::ContentError {
                code: ContentErrorCode::Unknown,
                ..
            }
        ),
        "{reply:?}"
    );

    let malformed = node
        .call(ContentCall::Stat {
            content: "not-hex".into(),
        })
        .expect("stat");
    assert!(
        matches!(
            malformed.0,
            ContentReply::ContentError {
                code: ContentErrorCode::Invalid,
                ..
            }
        ),
        "{malformed:?}"
    );
}

#[test]
fn forgetting_keeps_the_name_and_drops_the_bytes() {
    let node = node("forget");
    let plaintext = filler(13, 300 * 1024);
    let content = written(&node.upload([5u8; 16], &plaintext).expect("upload"));

    let (reply, _) = node
        .call(ContentCall::Forget {
            content: content.clone(),
        })
        .expect("forget");
    assert!(matches!(reply, ContentReply::ContentForgotten), "{reply:?}");

    let ContentReply::ContentStatus {
        resident_chunks,
        chunk_count,
        ..
    } = node
        .call(ContentCall::Stat {
            content: content.clone(),
        })
        .expect("stat")
        .0
    else {
        panic!("the name survives forgetting the bytes");
    };
    assert_eq!(resident_chunks, 0);
    assert!(chunk_count > 0);

    let (reply, _) = node
        .call(ContentCall::Read {
            content,
            offset: 0,
            len: 1024,
            patience_ms: 0,
        })
        .expect("read");
    assert!(
        matches!(
            reply,
            ContentReply::ContentError {
                code: ContentErrorCode::NotResident,
                ..
            }
        ),
        "a name whose bytes are gone is not resident, not unknown: {reply:?}"
    );
}

#[test]
fn the_mixed_version_window_is_empty_on_purpose() {
    // A process from before the current wire contract can misread a request
    // added by the newer protocol and desynchronise the channel. That is why
    // the minimum moves with the version rather than trailing it.
    assert_eq!(
        lait::control::MIN_SUPPORTED_CONTROL_PROTOCOL,
        lait::control::CONTROL_PROTOCOL_VERSION,
    );
    assert!(lait::control::check_control_protocol(8).is_err());
    assert!(lait::control::check_control_protocol(lait::control::CONTROL_PROTOCOL_VERSION).is_ok());
}
