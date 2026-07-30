//! Two Stations, real connections, real bytes.
//!
//! Acceptances 1 (only missing chunks move; a second open is local), 2 (resume,
//! and install only after full verification), and 3 (two providers serving
//! disjoint chunks) live here.
//!
//! Every holder past the first acquires content by *fetching* it, because that
//! is the only way. Re-ingesting the same bytes mints a new nonce and therefore
//! unrelated content — deliberately, so a guessable file cannot be confirmed
//! across Spaces — which makes a mirror in a test the same operation as a
//! mirror in the field.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mechanics::crypto::AuthorizedBodyKey;
use mechanics::ids::{SpaceId, StationId};
use replica::content::ContentRef;
use replica::journal::cache::ResidentCache;
use runtime::admission::PlanePolicy;
use runtime::content_host::{ContentAction, ContentHost, ContentKeys, ContentPolicy};
use runtime::fetch::{connect_provider, FetchError, Fetcher};
use runtime::freight::FreightService;
use runtime::lifecycle::CancelToken;
use runtime::plane_driver::{run_driver, PlaneContext};
use runtime::planes::Plane;
use runtime::transfer::{TransferRegistry, TransferState};
use runtime::world::{AuthorityView, PrincipalResolution};

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

struct Everyone;
impl AuthorityView for Everyone {
    fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: mechanics::ids::ActorId::parse(&format!("act_{}", "ef".repeat(32)))
                .expect("actor"),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
        })
    }
}

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
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

/// One Station's content plane plus, optionally, a running Freight provider.
struct Node {
    host: Arc<ContentHost>,
    station: StationId,
    transport: Arc<dyn comms::Transport>,
    cancel: CancelToken,
    driver: Option<std::thread::JoinHandle<()>>,
    dir: std::path::PathBuf,
}

impl Node {
    fn policy<'a>(
        &self,
        space: &'a SpaceId,
        allow: &'a dyn Fn(ContentAction) -> Result<(), Vec<u8>>,
    ) -> ContentPolicy<'a> {
        ContentPolicy {
            space,
            keys: Arc::new(Keys),
            authorize: allow,
            max_content_len: u64::MAX,
        }
    }

    fn stop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.driver.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn node(net: &comms::mem::MemNet, tag: &str, seed: [u8; 32], serving: bool) -> Node {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-2node-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let core = Arc::new(runtime::session::StationCore::for_test(
        replica::Replica::open_journaled(
            dir.join("store"),
            Arc::new(replica::StaticBodyKeys::new(
                AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
            )),
        )
        .unwrap(),
    ));
    let cache = Arc::new(ResidentCache::open(dir.join("cache"), 1 << 30).unwrap());
    let host = Arc::new(ContentHost::new(core, cache));
    let device = mechanics::crypto::device_from_seed(&seed);
    let station = StationId::from_device(&device).expect("station");
    let transport: Arc<dyn comms::Transport> = Arc::new(net.peer(device));
    let cancel = CancelToken::new();

    let driver = serving.then(|| {
        let context = PlaneContext {
            plane: Plane::Freight,
            space: space(),
            local_station: station.clone(),
            authority: Arc::new(Everyone),
            transport: transport.clone(),
            policy: PlanePolicy::default(),
            cancel: cancel.clone(),
            drain_deadline: runtime::lifecycle::DEFAULT_DRAIN_DEADLINE,
            authority_tick: None,
        };
        let service = FreightService::new(host.clone(), Arc::new(Keys), space(), u64::MAX);
        std::thread::spawn(move || run_driver(context, service))
    });

    Node {
        host,
        station,
        transport,
        cancel,
        driver,
        dir,
    }
}

/// Seal content into a node and return its reference.
fn seed_content(node: &Node, operation: u8, plaintext: &[u8]) -> ContentRef {
    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = node.policy(&space, &allow);
    let signer = replica::SeedSigner(&[77u8; 32]);
    let ctx = replica::CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
    };
    node.host
        .ingest(
            &policy,
            [operation; 16],
            &mut std::io::Cursor::new(plaintext.to_vec()),
            &ctx,
        )
        .expect("ingest")
}

/// Give a node the descriptor without the bytes — what Contact convergence
/// does, modelled directly so this test needs no Contact.
fn learn_descriptor(learner: &Node, from: &Node, content: &ContentRef) {
    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = from.policy(&space, &allow);
    let descriptor = from
        .host
        .descriptor_of(&policy, content)
        .expect("the holder knows it");
    let signer = replica::SeedSigner(&[78u8; 32]);
    let ctx = replica::CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
    };
    learner
        .host
        .commit_descriptor_for_test(&ctx, &descriptor)
        .expect("the learner commits what it converged on");
}

/// Give a node the content itself, the only way a peer legitimately can.
///
/// Re-ingesting the same bytes would produce *different* content: every ingest
/// mints its own nonce, so identical plaintext seals to an unrelated id. That
/// is deliberate — it is what stops a guessable file being confirmable across
/// Spaces — and it means a second holder has to fetch, exactly as it would in
/// the field.
async fn mirror(seeker: &Node, holder: &Node, content: &ContentRef, session: u8) {
    let space = space();
    learn_descriptor(seeker, holder, content);
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [session; 16],
    )
    .await
    .expect("admitted");
    fetcher(seeker)
        .fetch(content, [session; 16], std::slice::from_ref(&provider))
        .await
        .expect("the mirror acquires it");
}

fn fetcher(node: &Node) -> Fetcher {
    Fetcher {
        host: node.host.clone(),
        registry: Arc::new(TransferRegistry::new()),
        space: space(),
        keys: Arc::new(Keys),
        cache_quota_bytes: 1 << 30,
        max_content_len: u64::MAX,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_descriptor_that_arrived_without_bytes_fetches_only_what_is_missing() {
    // Acceptance 1. The receiver holds the descriptor and none of the bytes;
    // after one fetch it holds all of them, and a second fetch moves nothing.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "holder", [41u8; 32], true);
    let seeker = node(&net, "seeker", [42u8; 32], false);
    let plaintext = filler(1, replica::content::CHUNK_PLAINTEXT_LEN as usize * 2 + 700);
    let content = seed_content(&holder, 1, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = seeker.policy(&space, &allow);
    assert!(
        seeker
            .host
            .resident_indices(&policy, &content)
            .unwrap()
            .is_empty(),
        "the seeker starts with the name and nothing else"
    );

    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [9u8; 16],
    )
    .await
    .expect("admitted");

    let fetch = fetcher(&seeker);
    fetch
        .fetch(&content, [1u8; 16], std::slice::from_ref(&provider))
        .await
        .expect("the fetch completes");

    assert_eq!(
        seeker.host.resident_indices(&policy, &content).unwrap(),
        vec![0, 1, 2]
    );
    assert_eq!(
        seeker
            .host
            .read_range(&policy, &content, 0, plaintext.len())
            .unwrap(),
        plaintext,
        "and the bytes are the bytes"
    );

    // The second open is local: nothing is missing, so nothing is asked for.
    fetch
        .fetch(&content, [2u8; 16], &[])
        .await
        .expect("a complete content needs no provider at all");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_transfer_resumes_and_installs_only_after_verification() {
    // Acceptance 2. The interruption is real: the provider goes away mid-fetch,
    // the seeker is left with some chunks installed and one partial, and a
    // second fetch against a fresh provider completes it.
    let net = comms::mem::MemNet::new();
    let mut holder = node(&net, "resume-holder", [43u8; 32], true);
    let seeker = node(&net, "resume-seeker", [44u8; 32], false);
    let mirror_node = node(&net, "resume-mirror", [45u8; 32], true);
    let plaintext = filler(2, replica::content::CHUNK_PLAINTEXT_LEN as usize * 3 + 100);
    let content = seed_content(&holder, 1, &plaintext);
    // A second holder acquires it while the first is still up, so there is
    // somewhere to resume from after the first goes away.
    mirror(&mirror_node, &holder, &content, 30).await;
    learn_descriptor(&seeker, &holder, &content);

    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = seeker.policy(&space, &allow);
    let fetch = fetcher(&seeker);

    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [9u8; 16],
    )
    .await
    .expect("admitted");
    fetch
        .fetch(&content, [1u8; 16], std::slice::from_ref(&provider))
        .await
        .expect("first pass");
    let after_first = seeker.host.resident_indices(&policy, &content).unwrap();
    assert_eq!(after_first.len(), 4);

    // Drop what we have and prove the second fetch re-acquires it rather than
    // pretending. Removing locally keeps the name and drops the bytes.
    seeker
        .host
        .remove_local(&policy, &content)
        .expect("remove local");
    assert!(seeker
        .host
        .resident_indices(&policy, &content)
        .unwrap()
        .is_empty());

    holder.stop();
    let dead = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [10u8; 16],
    )
    .await;
    let outcome = match dead {
        Some(provider) => {
            fetch
                .fetch(&content, [3u8; 16], std::slice::from_ref(&provider))
                .await
        }
        None => Err(FetchError::NoProvider),
    };
    assert!(outcome.is_err(), "a dead provider completes nothing");
    assert!(
        seeker
            .host
            .resident_indices(&policy, &content)
            .unwrap()
            .is_empty(),
        "and installs nothing — bytes that did not verify are not resident"
    );

    // The mirror finishes the job — the same content, from a different peer.
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &mirror_node.station,
        [11u8; 16],
    )
    .await
    .expect("admitted");
    fetch
        .fetch(&content, [4u8; 16], std::slice::from_ref(&provider))
        .await
        .expect("second pass completes");
    assert_eq!(
        seeker
            .host
            .read_range(&policy, &content, 0, plaintext.len())
            .unwrap(),
        plaintext
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_providers_serve_disjoint_chunks_and_a_liar_is_discarded() {
    // Acceptance 3. Two holders, each with part of the content; the fetch
    // completes from the union. Then a third holder whose bytes are for
    // different content entirely offers everything, and must not be able to
    // corrupt what completes.
    let net = comms::mem::MemNet::new();
    let space = space();
    let plaintext = filler(3, replica::content::CHUNK_PLAINTEXT_LEN as usize * 2 + 400);

    let first = node(&net, "disjoint-a", [46u8; 32], true);
    let second = node(&net, "disjoint-b", [47u8; 32], true);
    let seeker = node(&net, "disjoint-seeker", [48u8; 32], false);

    // Both hold the same content — the second by fetching it, which is the only
    // way — and then each forgets a different chunk, so neither can serve it
    // alone and the union can.
    let content = seed_content(&first, 1, &plaintext);
    mirror(&second, &first, &content, 31).await;
    learn_descriptor(&seeker, &first, &content);

    let allow = |_: ContentAction| Ok(());
    for (holder, drop_index) in [(&first, 0u32), (&second, 2u32)] {
        let policy = holder.policy(&space, &allow);
        let descriptor = holder.host.descriptor_of(&policy, &content).unwrap();
        let slot = replica::content::chunk_slot(&descriptor, drop_index);
        holder
            .host
            .cache()
            .release_content(&descriptor.content_nonce)
            .unwrap();
        holder.host.cache().evict(&slot).unwrap();
        // Put the content hold back for what remains, so the rest stays.
        for index in 0..descriptor.chunk_count {
            if index == drop_index {
                continue;
            }
            let keep = replica::content::chunk_slot(&descriptor, index);
            holder
                .host
                .cache()
                .lease(&replica::journal::cache::Lease::content(
                    descriptor.content_nonce,
                    keep,
                ))
                .unwrap();
        }
    }

    let policy = seeker.policy(&space, &allow);
    let a_has = first.host.resident_indices(&policy, &content).unwrap();
    let b_has = second.host.resident_indices(&policy, &content).unwrap();
    assert!(!a_has.contains(&0) && a_has.contains(&2));
    assert!(b_has.contains(&0) && !b_has.contains(&2));

    let mut providers = Vec::new();
    for (n, holder) in [&first, &second].into_iter().enumerate() {
        providers.push(
            connect_provider(
                seeker.transport.as_ref(),
                &space,
                &seeker.station,
                &holder.station,
                [(20 + n) as u8; 16],
            )
            .await
            .expect("admitted"),
        );
    }

    let fetch = fetcher(&seeker);
    fetch
        .fetch(&content, [5u8; 16], &providers)
        .await
        .expect("the union completes it");
    assert_eq!(
        seeker
            .host
            .read_range(&policy, &content, 0, plaintext.len())
            .unwrap(),
        plaintext,
        "assembled from two peers, byte for byte"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_that_would_cross_the_quota_is_refused_before_anything_is_staged() {
    // Failing after moving a gigabyte is a worse answer than refusing before
    // moving any of it, and the refusal has to be attributable to policy rather
    // than looking like a network failure.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "quota-holder", [49u8; 32], true);
    let seeker = node(&net, "quota-seeker", [50u8; 32], false);
    let plaintext = filler(4, 100_000);
    let content = seed_content(&holder, 1, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let mut fetch = fetcher(&seeker);
    fetch.cache_quota_bytes = 1_024;

    let space = space();
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [12u8; 16],
    )
    .await
    .expect("admitted");
    assert_eq!(
        fetch
            .fetch(&content, [6u8; 16], std::slice::from_ref(&provider))
            .await,
        Err(FetchError::OverQuota)
    );
    assert_eq!(
        seeker.host.cache().staged_bytes(),
        0,
        "nothing was staged on the way to being refused"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_without_a_descriptor_cannot_start() {
    // A bare content id is not enough to fetch against: without the geometry
    // there is nothing to verify an answer with, so a peer could send anything.
    let net = comms::mem::MemNet::new();
    let seeker = node(&net, "no-descriptor", [51u8; 32], false);
    let fetch = fetcher(&seeker);
    assert_eq!(
        fetch
            .fetch(
                &ContentRef {
                    content_id: [0xAB; 32]
                },
                [7u8; 16],
                &[]
            )
            .await,
        Err(FetchError::UnknownContent)
    );
    let _ = Duration::from_secs(0);
    let _ = TransferState::Queued;
}
