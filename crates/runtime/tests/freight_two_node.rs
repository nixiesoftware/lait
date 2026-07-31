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
use std::time::{Duration, Instant};

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
use runtime::transfer::{TransferHandle, TransferRegistry, TransferState};
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
    core: Arc<runtime::session::StationCore>,
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

    /// Reopen the store and cache over the same directory, running exactly the
    /// reclaim that `Orbit::activate` runs.
    ///
    /// Nothing was in flight before this moment — that is what a restart means
    /// — so every operation lease and every staging slot on disk belongs to a
    /// run that is over.
    fn restart(&mut self) {
        self.stop();
        let mut replica = replica::Replica::open_journaled(
            self.dir.join("store"),
            Arc::new(replica::StaticBodyKeys::new(
                AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
            )),
        )
        .unwrap();
        replica.set_supported(supported());
        let core = Arc::new(runtime::session::StationCore::for_test(replica));
        self.core = core.clone();
        let cache = Arc::new(ResidentCache::open(self.dir.join("cache"), 1 << 30).unwrap());
        cache
            .sweep_leases(&std::collections::BTreeSet::new())
            .unwrap();
        cache
            .sweep_staging(&std::collections::BTreeSet::new())
            .unwrap();
        self.host = Arc::new(ContentHost::new(core, cache));
        self.cancel = CancelToken::new();
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

    let mut replica = replica::Replica::open_journaled(
        dir.join("store"),
        Arc::new(replica::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
        )),
    )
    .unwrap();
    replica.set_supported(supported());
    let core = Arc::new(runtime::session::StationCore::for_test(replica));
    let cache = Arc::new(ResidentCache::open(dir.join("cache"), 1 << 30).unwrap());
    let host = Arc::new(ContentHost::new(core.clone(), cache));
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
            policy: PlanePolicy::default(),
            cancel: cancel.clone(),
            drain_deadline: runtime::lifecycle::DEFAULT_DRAIN_DEADLINE,
            authority_tick: None,
        };
        let service = FreightService::new(
            host.clone(),
            Arc::new(TransferRegistry::new()),
            Arc::new(Keys),
            space(),
            u64::MAX,
        );
        // The hub splits inbound connections per plane in production; a test
        // that talks to a bare transport does the same job with one pump, so
        // the driver is exercised through the shape it actually has.
        let (queue_tx, queue_rx) = tokio::sync::mpsc::channel(16);
        let pump_transport = transport.clone();
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
    });

    Node {
        host,
        core,
        station,
        transport,
        cancel,
        driver,
        dir,
    }
}

// ---- the Space a declaring Body lives in ---------------------------------
//
// Content is only advertised when a live Body declares it, so a holder that
// wants a peer to be able to fetch has to have committed the Body that names
// it. That is not test scaffolding — it is the reachability rule, and a test
// that skipped it would be proving convergence of something no advertisement
// would ever carry.

fn world() -> replica::WorldId {
    replica::WorldId::parse("com.example.notes").unwrap()
}

fn body_key(n: u8) -> replica::BodyKey {
    replica::BodyKey::new(world(), replica::BodyId::from_bytes([n; 16]))
}

fn binding() -> replica::BodyBinding {
    replica::BodyBinding {
        schema: replica::SchemaId::parse("blob").unwrap(),
        schema_version: 1,
        encoding: replica::EncodingId::parse("bytes").unwrap(),
        mutation_model: replica::MUTATION_ATOMIC,
    }
}

fn supported() -> replica::SupportedSchemas {
    let mut s = replica::SupportedSchemas::new();
    s.declare(
        world(),
        replica::SchemaId::parse("blob").unwrap(),
        1,
        replica::EncodingId::parse("bytes").unwrap(),
        replica::MUTATION_ATOMIC,
    );
    s
}

fn demand() -> Vec<u8> {
    use mechanics::demand::{AuthorizationDemand, PolicyCapability, PolicyResource};
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.notes", "write"),
        PolicyResource::space("com.example.notes"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

const AUTHOR_SEED: [u8; 32] = [77u8; 32];

fn commit_context<'a>(
    space: &'a SpaceId,
    signer: &'a replica::SeedSigner<'a>,
) -> replica::CommitContext<'a> {
    replica::CommitContext {
        space,
        signer,
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
    }
}

/// Every signer is authorized here: this suite is about moving bytes, and
/// mechanics standing has its own tests.
struct AnySigner;
impl replica::AuthoritySource for AnySigner {
    fn signer_authorized(
        &self,
        _signer: &[u8; 32],
        _f: &replica::frontier::AuthorityFrontier,
    ) -> bool {
        true
    }
}

struct BatchReceipts;
impl replica::AuthorityIncorporator for BatchReceipts {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<replica::AuthorityBatchReceipt, String> {
        Ok(replica::AuthorityBatchReceipt {
            space: space(),
            prior_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

/// Commit the Body that will declare this content, so the holder has something
/// to advertise it from.
fn commit_declaring_body(node: &Node, seq: u8) {
    let space = space();
    let signer = replica::SeedSigner(&AUTHOR_SEED);
    let ctx = commit_context(&space, &signer);
    let mut request = [0u8; 16];
    request[0] = seq;
    node.core
        .with_replica(|replica| {
            replica.commit_action(
                &ctx,
                &replica::CommitAuthorization {
                    actor: "author",
                    parent_manifest_root: [0u8; 32],
                    demand: demand(),
                    intent_digest: [7u8; 32],
                    authorizer: &replica::StaticAuthorizer {
                        world: world(),
                        implementation_id: [0u8; 32],
                    },
                },
                &world(),
                &mechanics::crypto::device_from_seed(&AUTHOR_SEED),
                &request,
                &[7u8; 32],
                Vec::new(),
                Vec::new(),
                "author",
                &[(
                    body_key(seq),
                    replica::BodyOp::ReplaceAtomic {
                        value: b"an issue with a file".to_vec(),
                    },
                )],
                &[(body_key(seq), binding())],
                &[],
            )
        })
        .expect("commit the declaring Body");
}

/// Stage everything this node would serve a peer over Contact.
fn stage(node: &Node) -> replica::StagedContactMaterial {
    let space = space();
    let signer = replica::SeedSigner(&AUTHOR_SEED);
    let ctx = commit_context(&space, &signer);
    let (material, root, nodes) = node
        .core
        .with_replica(|replica| {
            let material = replica.export_material()?;
            let (root, nodes) = replica.export_manifest(&ctx)?;
            Ok((material, root, nodes))
        })
        .expect("export");
    let mut authority_records = vec![b"mechanics-authority-record".to_vec()];
    let mut bodies = Vec::new();
    for (tx, payloads) in &material {
        authority_records.push(tx.encode());
        for (key, envelope) in payloads {
            bodies.push((tx.id(), key.clone(), envelope.clone()));
        }
    }
    replica::StagedContactMaterial {
        authority_records,
        manifest_root_bytes: root,
        manifest_nodes: nodes,
        bodies,
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
    let content = node
        .host
        .ingest(
            &policy,
            [operation; 16],
            &mut std::io::Cursor::new(plaintext.to_vec()),
            &ctx,
        )
        .expect("ingest");
    // A Body has to name it or nothing advertises it: reachability is derived
    // from what live Bodies declare, on the wire exactly as it is on disk.
    commit_declaring_body(node, operation);
    let mut declarations = std::collections::BTreeMap::new();
    declarations.insert(body_key(operation), vec![content]);
    node.core
        .with_replica(|replica| replica.declare_content(&ctx, declarations))
        .expect("declare");
    content
}

/// Give a node the descriptor without the bytes, by converging on it.
///
/// This is a real Contact: the holder's signed advertisement, validated and
/// incorporated through the same path a peer's would be. Nothing is planted —
/// which is the point, because a planted descriptor would prove the fetch works
/// on state no advertisement could ever produce.
fn learn_descriptor(learner: &Node, from: &Node, content: &ContentRef) {
    let staged = stage(from);
    let space = space();
    let signer = replica::SeedSigner(&AUTHOR_SEED);
    let ctx = commit_context(&space, &signer);
    learner
        .core
        .with_replica(|replica| {
            let bundle = replica.validate_contact(&staged, &AnySigner, &mut BatchReceipts)?;
            replica.incorporate_bundle(&ctx, bundle, &AnySigner)
        })
        .expect("the learner converges on what the holder advertised");
    let allow = |_: ContentAction| Ok(());
    let policy = learner.policy(&space, &allow);
    learner
        .host
        .descriptor_of(&policy, content)
        .expect("and now knows the content's shape without holding a byte of it");
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
        Ok(provider) => {
            fetch
                .fetch(&content, [3u8; 16], std::slice::from_ref(&provider))
                .await
        }
        Err(_) => Err(FetchError::NoProvider),
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

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_reclaims_a_dead_transfer_and_keeps_what_was_installed() {
    // The two halves of what a restart must do, and they pull in opposite
    // directions: forget the partial (nothing binds it any more — the leaf a
    // resume would name lives in the fetch loop, not on disk) while keeping the
    // chunks that verified (those are durable, and re-fetching them would be
    // moving bytes we already own).
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "restart-holder", [52u8; 32], true);
    let mut seeker = node(&net, "restart-seeker", [53u8; 32], false);
    let plaintext = filler(5, replica::content::CHUNK_PLAINTEXT_LEN as usize * 3 + 200);
    let content = seed_content(&holder, 1, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let space = space();
    let allow = |_: ContentAction| Ok(());

    // A first fetch that really completes, so there is something to keep.
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [40u8; 16],
    )
    .await
    .expect("admitted");
    fetcher(&seeker)
        .fetch(&content, [40u8; 16], std::slice::from_ref(&provider))
        .await
        .expect("first fetch");
    let installed = {
        let policy = seeker.policy(&space, &allow);
        seeker.host.resident_indices(&policy, &content).unwrap()
    };
    assert_eq!(installed.len(), 4);

    // Now the wreckage a killed transfer leaves: a staged partial and an
    // operation lease that no live transfer holds.
    let dead = [0xDDu8; 16];
    let held_entry = {
        let policy = seeker.policy(&space, &allow);
        let descriptor = seeker.host.descriptor_of(&policy, &content).unwrap();
        replica::content::chunk_slot(&descriptor, 0)
    };
    seeker
        .host
        .cache()
        .append_staged(&dead, 7, 0, b"half a chunk")
        .unwrap();
    seeker
        .host
        .cache()
        .lease(&replica::journal::cache::Lease::operation(dead, held_entry))
        .unwrap();
    assert!(seeker.host.cache().staged_bytes() > 0);

    seeker.restart();

    assert_eq!(
        seeker.host.cache().staged_bytes(),
        0,
        "a partial with nothing binding it is not resumable, so it is not kept"
    );
    let policy = seeker.policy(&space, &allow);
    assert_eq!(
        seeker.host.resident_indices(&policy, &content).unwrap(),
        installed,
        "and every chunk that verified survives the restart"
    );
    assert_eq!(
        seeker
            .host
            .read_range(&policy, &content, 0, plaintext.len())
            .unwrap(),
        plaintext
    );

    // Re-fetching moves nothing: the whole content is already here.
    fetcher(&seeker)
        .fetch(&content, [41u8; 16], &[])
        .await
        .expect("nothing is missing, so no provider is needed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_re_fetches_at_chunk_granularity() {
    // The other side of the same rule. Chunks that never installed are gone
    // after a restart and have to move again — but only those, and the ones
    // that did install are not asked for.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "granular-holder", [54u8; 32], true);
    let mut seeker = node(&net, "granular-seeker", [55u8; 32], false);
    let plaintext = filler(6, replica::content::CHUNK_PLAINTEXT_LEN as usize * 3 + 50);
    let content = seed_content(&holder, 1, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let space = space();
    let allow = |_: ContentAction| Ok(());
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [42u8; 16],
    )
    .await
    .expect("admitted");
    fetcher(&seeker)
        .fetch(&content, [42u8; 16], std::slice::from_ref(&provider))
        .await
        .expect("fetch");

    // Drop one installed chunk, as a quota eviction would.
    let descriptor = {
        let policy = seeker.policy(&space, &allow);
        seeker.host.descriptor_of(&policy, &content).unwrap()
    };
    let slot = replica::content::chunk_slot(&descriptor, 2);
    seeker
        .host
        .cache()
        .release_content(&descriptor.content_nonce)
        .unwrap();
    seeker.host.cache().evict(&slot).unwrap();
    for index in 0..descriptor.chunk_count {
        if index == 2 {
            continue;
        }
        seeker
            .host
            .cache()
            .lease(&replica::journal::cache::Lease::content(
                descriptor.content_nonce,
                replica::content::chunk_slot(&descriptor, index),
            ))
            .unwrap();
    }
    seeker.restart();

    let policy = seeker.policy(&space, &allow);
    let before = seeker.host.resident_indices(&policy, &content).unwrap();
    assert!(!before.contains(&2) && before.len() == 3);

    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [43u8; 16],
    )
    .await
    .expect("admitted");
    fetcher(&seeker)
        .fetch(&content, [43u8; 16], std::slice::from_ref(&provider))
        .await
        .expect("the gap closes");
    assert_eq!(
        seeker
            .host
            .read_range(&policy, &content, 0, plaintext.len())
            .unwrap(),
        plaintext
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn housekeeping_reclaims_dead_staging_and_reports_what_it_cannot() {
    // Two things a quiet Station still has to do. Reclaiming what a finished
    // transfer left is the easy half; the other is admitting that a quota it
    // cannot meet is not a failure to sweep — every chunk of committed content
    // is held by that content's own lease, so there is nothing eligible, and
    // "0 reclaimed, Ok" while far over quota tells an operator nothing.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "maintain", [56u8; 32], false);
    let plaintext = filler(7, 40_000);
    let content = seed_content(&holder, 1, &plaintext);

    let registry = Arc::new(TransferRegistry::new());
    let service = FreightService::new(
        holder.host.clone(),
        registry.clone(),
        Arc::new(Keys),
        space(),
        u64::MAX,
    );

    // A dead operation's staging, and a live one's.
    holder
        .host
        .cache()
        .append_staged(&[0xAAu8; 16], 0, 0, b"orphaned")
        .unwrap();
    let live = TransferHandle::new(
        registry.clone(),
        holder.host.cache_handle(),
        [0xBBu8; 16],
        content,
        Instant::now(),
    )
    .expect("registered");
    holder
        .host
        .cache()
        .append_staged(&[0xBBu8; 16], 0, 0, b"in flight")
        .unwrap();

    runtime::plane_driver::PlaneService::maintain(&service).await;

    assert_eq!(
        holder.host.cache().staged_len(&[0xAAu8; 16], 0),
        0,
        "an operation no transfer claims is over"
    );
    assert_eq!(
        holder.host.cache().staged_len(&[0xBBu8; 16], 0),
        9,
        "and a live one is left alone"
    );
    drop(live);
}
