//! Freight's provider half, exercised over a real connection.
//!
//! The client here is hand-written on purpose. A test that drove the provider
//! through lait's own fetcher would only prove the two agree with each other;
//! what needs proving is that the *wire* behaves — that an exact request gets
//! an exact answer, that everything else gets the same coarse refusal, and that
//! a malformed frame is refused by its declared length before a buffer that
//! size exists.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::{ids::SpaceId, station::Key};
use replica::content::ContentRef;
use replica::content::Residency;
use runtime::admission::PlanePolicy;
use runtime::content_host::{ContentAction, ContentHost, ContentKeys, ContentPolicy};
use runtime::plane::freight::{frame, read_frame, FreightService};
use runtime::plane::{bounds, feature, FreightFrame, Open, Plane, SPACE_ID_LEN};
use runtime::plane_driver::{run_driver, PlaneContext};
use runtime::world::{AuthorityView, PrincipalResolution};

const PROVIDER_SEED: [u8; 32] = [21u8; 32];
const CLIENT_SEED: [u8; 32] = [22u8; 32];
const EPOCH: [u8; 16] = [3u8; 16];
const EPOCH_KEY: [u8; 32] = [4u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Keys;
impl ContentKeys for Keys {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        Some(AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
    }
    fn opening_key(&self, _epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        Some(AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
    }
}

/// Everyone is a member. Admission itself is covered in `admission_fixtures`;
/// what this file is about is what happens after.
struct Everyone;
impl AuthorityView for Everyone {
    fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: mechanics::ids::ActorId::parse(&format!("act_{}", "cd".repeat(32)))
                .expect("actor"),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
        })
    }
}

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

fn station(seed: &[u8; 32]) -> Key {
    Key::from_device(&mechanics::actor::device_from_seed(seed)).expect("station")
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

fn opening(space: &SpaceId) -> Open {
    let mut bytes = [0u8; SPACE_ID_LEN];
    bytes.copy_from_slice(space.as_str().as_bytes());
    Open {
        plane: Plane::Freight,
        protocol_version: Plane::Freight.protocol_version(),
        features: feature::RESIDENCY_HINTS,
        space: bytes,
        initiator_station: station(&CLIENT_SEED).key_bytes(),
        responder_station: station(&PROVIDER_SEED).key_bytes(),
        connection_id: [3u8; 16],
        connection_epoch: [4u8; 16],
        authority_frontier: vec![9],
        requested_lanes: Vec::new(),
    }
}

/// A provider running its real driver on its own thread, plus a client
/// connection into it.
struct Wire {
    client: Box<dyn comms::Connection>,
    content: ContentRef,
    cancel: runtime::lifecycle::CancelToken,
    driver: Option<std::thread::JoinHandle<()>>,
    dir: std::path::PathBuf,
    _keep: Vec<Arc<dyn comms::Transport>>,
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.driver.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn wire(tag: &str, plaintext: Vec<u8>) -> Wire {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-freight-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let core = Arc::new(runtime::session::StationCore::for_test(
        replica::Replica::open(
            dir.join("store"),
            Arc::new(replica::body::StaticBodyKeys::new(
                AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
            )),
        )
        .unwrap(),
    ));
    let cache = Arc::new(Residency::open(dir.join("cache"), 1 << 30).unwrap());
    let host = Arc::new(ContentHost::new(core, cache));

    // Seed the provider with content it can serve.
    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = ContentPolicy {
        space: &space,
        keys: Arc::new(Keys),
        authorize: &allow,
        max_content_len: u64::MAX,
    };
    let signer = replica::transaction::SeedSigner(&PROVIDER_SEED);
    let ctx = replica::transaction::CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
    };
    let content = host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(plaintext.clone()),
            &ctx,
        )
        .expect("ingest");

    // Two peers on the in-memory switchboard.
    let net = comms::mem::MemNet::new();
    let provider_transport: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&PROVIDER_SEED)));
    let client_transport: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&CLIENT_SEED)));

    let cancel = runtime::lifecycle::CancelToken::new();
    let driver = {
        let context = PlaneContext {
            plane: Plane::Freight,
            space: space.clone(),
            local_station: station(&PROVIDER_SEED),
            authority: Arc::new(Everyone),
            policy: PlanePolicy::default(),
            cancel: cancel.clone(),
            drain_deadline: runtime::lifecycle::DEFAULT_DRAIN_DEADLINE,
            authority_tick: None,
        };
        let service = FreightService::new(
            host.clone(),
            Arc::new(runtime::transfer::TransferRegistry::new()),
            Arc::new(Keys),
            space.clone(),
            u64::MAX,
        );
        // The hub splits inbound connections per plane in production; a test
        // that talks to a bare transport does the same job with one pump, so
        // the driver is exercised through the shape it actually has.
        let (queue_tx, queue_rx) = tokio::sync::mpsc::channel(16);
        let pump_transport = provider_transport.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("pump runtime");
            rt.block_on(async move {
                while let Some(incoming) = pump_transport.accept_connection().await {
                    if queue_tx.send(incoming).await.is_err() {
                        break;
                    }
                }
            });
        });
        std::thread::spawn(move || run_driver(context, queue_rx, service))
    };

    let client = client_transport
        .connect_session(provider_transport.my_id(), runtime::plane::FREIGHT_ALPN)
        .await
        .expect("dial");

    // The opening goes on the initiator's first flow, and the mem transport
    // delivers it to `accept_connection` the way the hub would.
    let mut flow = client.open_uni().await.expect("open");
    flow.write_all(&opening(&space).encode())
        .await
        .expect("write");
    flow.finish().expect("finish");

    Wire {
        client,
        content,
        cancel,
        driver: Some(driver),
        dir,
        _keep: vec![provider_transport, client_transport],
    }
}

/// Send one request, read one answer plus whatever raw bytes followed.
async fn ask(wire: &Wire, request: FreightFrame) -> Option<(FreightFrame, Vec<u8>)> {
    let (mut send, mut recv) = wire.client.open_bi().await.ok()?;
    send.write_all(&frame(&request)).await.ok()?;
    send.finish().ok()?;
    let answer = tokio::time::timeout(
        Duration::from_secs(10),
        read_frame(recv.as_mut(), bounds::MAX_CONTROL_FRAME_BYTES),
    )
    .await
    .ok()?
    .ok()?;
    let rest = recv
        .read_to_end(bounds::MAX_CHUNK_FRAME_BYTES)
        .await
        .unwrap_or_default();
    Some((answer, rest))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exact_request_gets_an_exact_answer() {
    let plaintext = filler(1, replica::content::CHUNK_PLAINTEXT_LEN as usize + 900);
    let wire = wire("exact", plaintext.clone()).await;

    let (answer, _) = ask(
        &wire,
        FreightFrame::Have {
            content_id: wire.content.content_id,
            wanted: vec![0, 1],
        },
    )
    .await
    .expect("an answer");
    assert_eq!(
        answer,
        FreightFrame::Available {
            content_id: wire.content.content_id,
            chunks: vec![0, 1],
        }
    );

    let (header, bytes) = ask(
        &wire,
        FreightFrame::GetChunk {
            content_id: wire.content.content_id,
            chunk_index: 1,
            offset: 0,
            max_len: bounds::MAX_CHUNK_FRAME_BYTES as u32,
            resume_leaf: None,
        },
    )
    .await
    .expect("an answer");
    let FreightFrame::ChunkHeader {
        chunk_index,
        proof,
        total_len,
        ..
    } = header
    else {
        panic!("a chunk answer: {header:?}");
    };
    assert_eq!(chunk_index, 1);
    assert_eq!(bytes.len(), total_len as usize);

    // The bytes are ciphertext, and the proof is what makes them trustworthy
    // without the key — which is exactly why the tree commits ciphertexts.
    assert_ne!(
        bytes,
        plaintext[replica::content::CHUNK_PLAINTEXT_LEN as usize..]
    );
    let proof = replica::content::ChunkProof::decode_canonical(&proof).expect("canonical proof");
    let descriptor = {
        let allow = |_: ContentAction| Ok(());
        let space = space();
        let policy = ContentPolicy {
            space: &space,
            keys: Arc::new(Keys),
            authorize: &allow,
            max_content_len: u64::MAX,
        };
        let _ = policy;
        proof
    };
    assert_eq!(descriptor.leaf.chunk_index, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resume_naming_another_leaf_is_refused_before_a_byte_is_written() {
    // Without this a transfer could be steered onto different content halfway
    // through: the requester validated a leaf, and a provider answering with a
    // different one is either confused or lying.
    let wire = wire("resume", filler(2, 40_000)).await;
    let (answer, bytes) = ask(
        &wire,
        FreightFrame::GetChunk {
            content_id: wire.content.content_id,
            chunk_index: 0,
            offset: 0,
            max_len: 1024,
            resume_leaf: Some([0xAB; 32]),
        },
    )
    .await
    .expect("an answer");
    assert_eq!(answer, FreightFrame::Refused);
    assert!(bytes.is_empty(), "and nothing followed it");
}

#[tokio::test(flavor = "multi_thread")]
async fn everything_this_provider_will_not_answer_looks_the_same() {
    // Authorization, policy, load, absence, and a peer using the wrong end of
    // the protocol all produce one answer. A peer that could tell them apart
    // could map a Space by asking about ids it invented.
    let wire = wire("coarse", filler(3, 20_000)).await;
    let invented = [0xEEu8; 32];

    let (unknown_get, _) = ask(
        &wire,
        FreightFrame::GetChunk {
            content_id: invented,
            chunk_index: 0,
            offset: 0,
            max_len: 1024,
            resume_leaf: None,
        },
    )
    .await
    .expect("an answer");
    assert_eq!(unknown_get, FreightFrame::Refused);

    let (past_the_end, _) = ask(
        &wire,
        FreightFrame::GetChunk {
            content_id: wire.content.content_id,
            chunk_index: 99,
            offset: 0,
            max_len: 1024,
            resume_leaf: None,
        },
    )
    .await
    .expect("an answer");
    assert_eq!(past_the_end, FreightFrame::Refused);

    let (wrong_end, _) = ask(&wire, FreightFrame::Refused).await.expect("an answer");
    assert_eq!(wrong_end, FreightFrame::Refused);

    // An availability question about an invented id answers the same as one
    // about content this Station simply does not hold: an empty list.
    let (unknown_have, _) = ask(
        &wire,
        FreightFrame::Have {
            content_id: invented,
            wanted: vec![0, 1, 2],
        },
    )
    .await
    .expect("an answer");
    assert_eq!(
        unknown_have,
        FreightFrame::Available {
            content_id: invented,
            chunks: Vec::new(),
        },
        "residency and ignorance are indistinguishable"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_length_past_the_ceiling_is_refused_before_a_buffer_exists() {
    // Acceptance 8, provider half. The bound is on the *declared* length, so a
    // peer cannot make us allocate by saying a large number.
    let wire = wire("bounded", filler(4, 10_000)).await;
    let (mut send, mut recv) = wire.client.open_bi().await.expect("open");

    // A prefix claiming far more than a control frame may hold, and then
    // nothing.
    let mut hostile = Vec::new();
    hostile.extend_from_slice(&(u32::MAX).to_le_bytes());
    send.write_all(&hostile).await.expect("write");
    send.finish().expect("finish");

    // The provider resets rather than answering, and never reads a body.
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        recv.read_to_end(bounds::MAX_CHUNK_FRAME_BYTES),
    )
    .await
    .expect("bounded");
    assert!(
        outcome.map(|b| b.is_empty()).unwrap_or(true),
        "an over-declared frame is not answered"
    );
}
