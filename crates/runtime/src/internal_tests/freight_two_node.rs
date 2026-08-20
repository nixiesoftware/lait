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
use tokio::time::Instant;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::{ids::SpaceId, station::Key};
use replica::content::ContentRef;
use replica::content::Residency;
use runtime::admission::PlanePolicy;
use runtime::content_host::{Acquisition, ContentAction, ContentHost, ContentKeys, ContentPolicy};
use runtime::fetch::{connect_provider, Failure, Fetcher};
use runtime::lifecycle::CancelToken;
use runtime::plane::freight::FreightService;
use runtime::plane::Plane;
use runtime::plane_driver::{run_driver, PlaneContext};
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
    station: Key,
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
        let mut replica = replica::Replica::open(
            self.dir.join("store"),
            Arc::new(replica::body::StaticBodyKeys::new(
                AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
            )),
        )
        .unwrap();
        replica.set_supported(supported());
        let core = Arc::new(runtime::session::StationCore::for_test(replica));
        self.core = core.clone();
        let cache = Arc::new(Residency::open(self.dir.join("cache"), 1 << 30).unwrap());
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
    node_with_quota(net, tag, seed, serving, 1 << 30)
}

/// A node whose cache quota is its own, for the demand-paging tests.
///
/// The residency's quota and the fetcher's have to be the same number: the
/// fetcher refuses against one and the sweep reclaims against the other, so a
/// tighter fetcher quota would refuse forever against a cache that never feels
/// pressure.
fn node_with_quota(
    net: &comms::mem::MemNet,
    tag: &str,
    seed: [u8; 32],
    serving: bool,
    quota: u64,
) -> Node {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-2node-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut replica = replica::Replica::open(
        dir.join("store"),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
        )),
    )
    .unwrap();
    replica.set_supported(supported());
    let core = Arc::new(runtime::session::StationCore::for_test(replica));
    let cache = Arc::new(Residency::open(dir.join("cache"), quota).unwrap());
    let host = Arc::new(ContentHost::new(core.clone(), cache));
    let device = mechanics::actor::device_from_seed(&seed);
    let station = Key::from_device(&device).expect("station");
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

fn world() -> replica::body::WorldId {
    replica::body::WorldId::parse("com.example.notes").unwrap()
}

fn body_key(n: u8) -> replica::body::BodyKey {
    replica::body::BodyKey::new(world(), replica::body::BodyId::from_bytes([n; 16]))
}

fn binding() -> replica::body::BodyBinding {
    replica::body::BodyBinding {
        schema: replica::body::SchemaId::parse("blob").unwrap(),
        schema_version: 1,
        encoding: replica::body::EncodingId::parse("bytes").unwrap(),
        mutation_model: replica::body::MUTATION_ATOMIC,
    }
}

fn supported() -> replica::body::SupportedSchemas {
    let mut s = replica::body::SupportedSchemas::new();
    s.declare(
        world(),
        replica::body::SchemaId::parse("blob").unwrap(),
        1,
        replica::body::EncodingId::parse("bytes").unwrap(),
        replica::body::MUTATION_ATOMIC,
    );
    s
}

fn demand() -> Vec<u8> {
    use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.notes", "write"),
        Resource::root("com.example.notes"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

const AUTHOR_SEED: [u8; 32] = [77u8; 32];

fn commit_context<'a>(
    space: &'a SpaceId,
    signer: &'a replica::transaction::SeedSigner<'a>,
) -> replica::transaction::CommitContext<'a> {
    replica::transaction::CommitContext {
        space,
        signer,
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
    }
}

/// Every signer is authorized here: this suite is about moving bytes, and
/// mechanics standing has its own tests.
struct AnySigner;
impl replica::transaction::AuthoritySource for AnySigner {
    fn signer_authorized(
        &self,
        _signer: &[u8; 32],
        _f: &replica::frontier::AuthorityFrontier,
    ) -> bool {
        true
    }
}

struct BatchReceipts;
impl replica::convergence::AuthorityIncorporator for BatchReceipts {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
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
    let signer = replica::transaction::SeedSigner(&AUTHOR_SEED);
    let ctx = commit_context(&space, &signer);
    let mut request = [0u8; 16];
    request[0] = seq;
    let changed = body_key(seq);
    node.core
        .with_replica_bodies(std::slice::from_ref(&changed), |replica| {
            replica.commit_action(
                &ctx,
                &replica::transaction::CommitAuthorization {
                    actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
                    parent_manifest_root: [0u8; 32],
                    demand: demand(),
                    intent_digest: [7u8; 32],
                    authorizer: &replica::transaction::StaticAuthorizer {
                        world: world(),
                        implementation_id: [0u8; 32],
                    },
                },
                &world(),
                &mechanics::actor::device_from_seed(&AUTHOR_SEED),
                &request,
                &[7u8; 32],
                Vec::new(),
                Vec::new(),
                "author",
                &[(
                    body_key(seq),
                    replica::body::Op::ReplaceAtomic {
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
fn stage(node: &Node) -> replica::convergence::StagedContactMaterial {
    let space = space();
    let signer = replica::transaction::SeedSigner(&AUTHOR_SEED);
    let ctx = commit_context(&space, &signer);
    let (material, root, nodes) = node
        .core
        .with_replica_read(|replica| {
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
    replica::convergence::StagedContactMaterial {
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
    let signer = replica::transaction::SeedSigner(&[77u8; 32]);
    let ctx = replica::transaction::CommitContext {
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
    let changed = body_key(operation);
    node.core
        .with_replica_bodies(std::slice::from_ref(&changed), |replica| {
            replica.declare_content(&ctx, declarations)
        })
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
    let signer = replica::transaction::SeedSigner(&AUTHOR_SEED);
    let ctx = commit_context(&space, &signer);
    learner
        .core
        .with_replica_convergence(|replica| {
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
        .fetch(
            content,
            [session; 16],
            std::slice::from_ref(&provider),
            &anyone,
        )
        .await
        .expect("the mirror acquires it");
}

/// The fetcher's authorization, chosen at the call site because it has to be.
///
/// This suite is about moving bytes; Mechanics standing has its own tests. A
/// caller with an actor behind it passes that actor's predicate instead.
fn anyone(_: ContentAction<'_>) -> Result<(), Vec<u8>> {
    Ok(())
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
        .fetch(
            &content,
            [1u8; 16],
            std::slice::from_ref(&provider),
            &anyone,
        )
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
        .fetch(&content, [2u8; 16], &[], &anyone)
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
        .fetch(
            &content,
            [1u8; 16],
            std::slice::from_ref(&provider),
            &anyone,
        )
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
                .fetch(
                    &content,
                    [3u8; 16],
                    std::slice::from_ref(&provider),
                    &anyone,
                )
                .await
        }
        Err(_) => Err(Failure::NoProvider),
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
        .fetch(
            &content,
            [4u8; 16],
            std::slice::from_ref(&provider),
            &anyone,
        )
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
                .hold_content(descriptor.content_nonce, keep)
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
        .fetch(&content, [5u8; 16], &providers, &anyone)
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
            .fetch(
                &content,
                [6u8; 16],
                std::slice::from_ref(&provider),
                &anyone
            )
            .await,
        Err(Failure::OverQuota)
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
                &[],
                &anyone
            )
            .await,
        Err(Failure::UnknownContent)
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
        .fetch(
            &content,
            [40u8; 16],
            std::slice::from_ref(&provider),
            &anyone,
        )
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
        .hold_operation(dead, held_entry)
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
        .fetch(&content, [41u8; 16], &[], &anyone)
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
        .fetch(
            &content,
            [42u8; 16],
            std::slice::from_ref(&provider),
            &anyone,
        )
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
            .hold_content(
                descriptor.content_nonce,
                replica::content::chunk_slot(&descriptor, index),
            )
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
        .fetch(
            &content,
            [43u8; 16],
            std::slice::from_ref(&provider),
            &anyone,
        )
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

#[tokio::test(flavor = "multi_thread")]
async fn a_named_window_moves_and_the_rest_of_the_content_stays_missing() {
    // What a playhead asks for. Answering "one second of this film" by moving
    // the film is the defect this entry point exists to prevent, and the proof
    // is that the chunks nobody asked about are still absent afterwards.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "window-holder", [57u8; 32], true);
    let seeker = node(&net, "window-seeker", [58u8; 32], false);
    let chunk = replica::content::CHUNK_PLAINTEXT_LEN as usize;
    let plaintext = filler(8, chunk * 3 + 90);
    let content = seed_content(&holder, 1, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = seeker.policy(&space, &allow);
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [13u8; 16],
    )
    .await
    .expect("admitted");

    fetcher(&seeker)
        .fetch_chunks(
            &content,
            &[2],
            std::slice::from_ref(&provider),
            [60u8; 16],
            Acquisition::Keep,
            &CancelToken::new(),
            &anyone,
        )
        .await
        .expect("the window arrives");

    assert_eq!(
        seeker.host.resident_indices(&policy, &content).unwrap(),
        vec![2],
        "one chunk was named, so one chunk moved"
    );
    assert_eq!(
        seeker
            .host
            .read_range(&policy, &content, (chunk * 2) as u64, chunk)
            .unwrap(),
        plaintext[chunk * 2..chunk * 3],
        "and it reads as the bytes that belong at that offset"
    );
    assert!(
        matches!(
            seeker.host.read_range(&policy, &content, 0, 16),
            Err(runtime::content_host::Failure::NotResident)
        ),
        "the rest of the content is still a hole, and says so"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_is_admitted_where_the_whole_content_is_refused() {
    // The blocker. Pricing admission on the whole content refuses a film
    // outright on a Station whose cache would hold the scene being watched
    // several times over, however few chunks the playhead actually needs.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "window-quota-holder", [59u8; 32], true);
    let seeker = node(&net, "window-quota-seeker", [60u8; 32], false);
    let chunk = replica::content::CHUNK_PLAINTEXT_LEN as u64;
    let plaintext = filler(9, chunk as usize * 3 + 50);
    let content = seed_content(&holder, 1, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = seeker.policy(&space, &allow);
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [14u8; 16],
    )
    .await
    .expect("admitted");

    let mut fetch = fetcher(&seeker);
    fetch.cache_quota_bytes = chunk + 65_536;
    assert_eq!(
        fetch
            .fetch(
                &content,
                [61u8; 16],
                std::slice::from_ref(&provider),
                &anyone
            )
            .await,
        Err(Failure::OverQuota),
        "the whole content does not fit, and is refused before anything is staged"
    );
    assert_eq!(seeker.host.cache().staged_bytes(), 0);

    fetch
        .fetch_chunks(
            &content,
            &[1],
            std::slice::from_ref(&provider),
            [62u8; 16],
            Acquisition::Keep,
            &CancelToken::new(),
            &anyone,
        )
        .await
        .expect("one chunk of it fits, and that is what was asked for");
    assert_eq!(
        seeker.host.resident_indices(&policy, &content).unwrap(),
        vec![1]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_fetch_leaves_no_staged_bytes_and_no_lease() {
    // A withdrawal is its own answer. What it must not be is disk: an
    // abandoned transfer that kept its staging and its lease is space nothing
    // will ever reclaim, because nothing is left to say the operation is over.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "cancel-holder", [61u8; 32], true);
    let seeker = node(&net, "cancel-seeker", [62u8; 32], false);
    let plaintext = filler(10, replica::content::CHUNK_PLAINTEXT_LEN as usize * 2 + 40);
    let content = seed_content(&holder, 1, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let space = space();
    let allow = |_: ContentAction| Ok(());
    let policy = seeker.policy(&space, &allow);
    let operation = [63u8; 16];
    let leased = {
        let descriptor = seeker.host.descriptor_of(&policy, &content).unwrap();
        replica::content::chunk_slot(&descriptor, 0)
    };
    seeker
        .host
        .cache()
        .append_staged(&operation, 0, 0, b"half a chunk")
        .unwrap();
    seeker
        .host
        .cache()
        .hold_operation(operation, leased)
        .unwrap();

    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [15u8; 16],
    )
    .await
    .expect("admitted");

    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        fetcher(&seeker)
            .fetch_chunks(
                &content,
                &[0, 1, 2],
                std::slice::from_ref(&provider),
                operation,
                Acquisition::Keep,
                &cancel,
                &anyone,
            )
            .await,
        Err(Failure::Cancelled),
        "withdrawn, not incomplete — only one of those is worth retrying"
    );
    assert_eq!(
        seeker.host.cache().staged_bytes(),
        0,
        "nothing staged survives the transfer that staged it"
    );
    assert!(
        !seeker.host.cache().is_held(&leased).unwrap(),
        "and the operation's lease went with it"
    );
    assert!(seeker
        .host
        .resident_indices(&policy, &content)
        .unwrap()
        .is_empty());
}

// ===========================================================================
// The driver, under a fault schedule and a stopped clock, at the same time.
// ===========================================================================
//
// Everything above runs the real drivers on real OS threads under a
// multi-thread runtime, which is the right shape for testing that bytes move.
// It is the wrong shape for a simulation: `tokio::time::pause` requires the
// current_thread runtime, so a driver on its own thread cannot have its clock
// stopped.
//
// So this is the same stack, assembled differently — every driver cooperating
// on ONE thread inside a `LocalSet`, which is what lets a test hold the clock
// still while a lossy network drops things.
//
// It is the piece the other simulations do not have. `convergence_simulation`
// controls a schedule with no clock; `paused_clock` and `driver_beat` control a
// clock with no faults; `network_simulation_tests` controls faults with no
// driver. This controls all three, over the real `plane_driver::drive`.
//
// ## Where the faults reach, and where they do not
//
// `MemNet` decides per CONNECTION, not per frame: `connect`, `connect_session`
// and gossip delivery are gated, and the byte stream inside an established
// connection is not. Measured — a whole fetch shows `sent=1`, because the
// transfer happens within one admitted session.
//
// That bound is worth naming rather than quietly living with, and it is also
// why per-frame faults are not the obvious next thing to build: what they would
// buy is partial-transfer-under-loss, and
// `an_interrupted_transfer_resumes_and_installs_only_after_verification` above
// already covers resume by removing the provider mid-fetch. The uncovered
// ground is narrower than it looks.

/// One station's parts, without the driver started.
///
/// `node()` above spawns its pump and driver onto OS threads. That is exactly
/// what cannot happen here, so this hands the caller the pieces and lets it
/// `spawn_local` them into whichever `LocalSet` is holding the clock.
struct LocalStation {
    node: Node,
    context: PlaneContext,
    service: FreightService,
}

fn local_station(net: &comms::mem::MemNet, tag: &str, seed: [u8; 32]) -> LocalStation {
    let node = node(net, tag, seed, false);
    let context = PlaneContext {
        plane: Plane::Freight,
        space: space(),
        local_station: node.station.clone(),
        authority: Arc::new(Everyone),
        policy: PlanePolicy::default(),
        cancel: node.cancel.clone(),
        drain_deadline: runtime::lifecycle::DEFAULT_DRAIN_DEADLINE,
        authority_tick: None,
    };
    let service = FreightService::new(
        node.host.clone(),
        Arc::new(TransferRegistry::new()),
        Arc::new(Keys),
        space(),
        u64::MAX,
    );
    LocalStation {
        node,
        context,
        service,
    }
}

/// Start a station's inbound pump and plane driver on the current `LocalSet`.
///
/// The pump is what `TransportHub` does in production — split inbound
/// connections per plane. A bare `MemTransport` has one undivided door, so a
/// test does the same job with six lines, exactly as `node()` does with a
/// thread.
fn spawn_local_station(station: LocalStation) -> Node {
    let (queue_tx, queue_rx) = tokio::sync::mpsc::channel(16);
    let transport = station.node.transport.clone();
    tokio::task::spawn_local(async move {
        while let Some(incoming) = transport.accept_connection().await {
            if queue_tx.send(incoming).await.is_err() {
                break;
            }
        }
    });
    tokio::task::spawn_local(runtime::plane_driver::drive(
        station.context,
        queue_rx,
        station.service,
    ));
    station.node
}

/// A fetch completes over a network that is dropping things, with the clock
/// held still.
///
/// The clock matters as much as the faults. Freight's retries and deadlines are
/// written in `Duration`, so a lossy transfer only finishes if time advances —
/// and against a real clock that means either a slow test or a flaky one. Here
/// time advances because the test says so, in steps, and the whole thing runs
/// in milliseconds.
///
/// `Faults::PERFECT` first: a simulation that has never been watched succeed is
/// a simulation nobody can trust to fail meaningfully.
#[tokio::test(start_paused = true)]
async fn a_fetch_completes_under_a_paused_clock() {
    let net = comms::mem::MemNet::seeded(0xD12E, comms::mem::Faults::PERFECT);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let holder = spawn_local_station(local_station(&net, "sim-holder", [21u8; 32]));
            let seeker = spawn_local_station(local_station(&net, "sim-seeker", [22u8; 32]));
            tokio::task::yield_now().await;

            let content = seed_content(&holder, 1, b"bytes crossing a stopped clock");
            learn_descriptor(&seeker, &holder, &content);

            let provider = connect_provider(
                seeker.transport.as_ref(),
                &space(),
                &seeker.station,
                &holder.station,
                [1u8; 16],
            )
            .await
            .expect("the provider admits the seeker");

            let fetched = fetcher(&seeker)
                .fetch(
                    &content,
                    [1u8; 16],
                    std::slice::from_ref(&provider),
                    &anyone,
                )
                .await;
            assert!(
                fetched.is_ok(),
                "a fetch over a perfect network should complete: {fetched:?}"
            );

            holder.cancel.cancel();
            seeker.cancel.cancel();
            tokio::time::advance(Duration::from_millis(100)).await;
        })
        .await;
}

/// A dropped dial is reported as unreachable — not a hang, not a silent
/// success.
///
/// This is the design decision in `MemNet::connect_session` observed from
/// above. A dropped dial there returns a live handle nobody holds the other end
/// of, because that is what a lost SYN looks like to a dialer: no error, no
/// answer. The question this settles is whether the layer above turns that into
/// something a caller can act on, and it does — `connect_provider` reports
/// `Unreachable` rather than parking forever on a peer that will never speak.
///
/// Seed 4242 drops the dial; the counters are asserted so the test cannot pass
/// because nothing was dropped at all.
#[tokio::test(start_paused = true)]
async fn a_dropped_dial_is_unreachable_rather_than_a_hang() {
    let net = comms::mem::MemNet::seeded(2, comms::mem::Faults::LOSSY);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let holder = spawn_local_station(local_station(&net, "drop-holder", [31u8; 32]));
            let seeker = spawn_local_station(local_station(&net, "drop-seeker", [32u8; 32]));
            tokio::task::yield_now().await;

            let content = seed_content(&holder, 1, b"bytes nobody will reach");
            learn_descriptor(&seeker, &holder, &content);

            let provider = connect_provider(
                seeker.transport.as_ref(),
                &space(),
                &seeker.station,
                &holder.station,
                [1u8; 16],
            )
            .await;

            let delivered = net.delivered();
            assert!(
                delivered.dropped > 0,
                "this seed is supposed to drop the dial; it dropped nothing, so                  the assertion below would prove nothing"
            );
            // `Provider` has no Debug, so match rather than format the Ok side.
            match provider {
                Err(runtime::fetch::ProviderRefusal::Unreachable) => {}
                Err(other) => panic!("expected Unreachable, got {other:?}"),
                Ok(_) => panic!("the dial was dropped; it must not have succeeded"),
            }

            holder.cancel.cancel();
            seeker.cancel.cancel();
            tokio::time::advance(Duration::from_millis(100)).await;
        })
        .await;
}

/// What a cursor asked for, so the driver can go and get it.
///
/// Duct tape, and named as such: the production supply spawns the fetch and
/// answers immediately, which needs a provider-assembly path that does not
/// exist yet. This records the ask and lets the test do the fetching, which
/// exercises the same chain — cursor asks, window is fetched, cursor reads —
/// without inventing that path under a test's convenience.
#[derive(Default)]
struct Wanted(std::sync::Mutex<Vec<u32>>);

impl runtime::content_cursor::ChunkSupply for Wanted {
    fn request(
        &self,
        _content: &ContentRef,
        _operation: [u8; 16],
        chunks: &[u32],
    ) -> runtime::content_cursor::Gap {
        if let Ok(mut wanted) = self.0.lock() {
            wanted.extend_from_slice(chunks);
        }
        runtime::content_cursor::Gap::Fetching
    }

    fn abandon(&self, _content: &ContentRef, _operation: [u8; 16]) {}
}

impl Wanted {
    fn take(&self) -> Vec<u32> {
        self.0
            .lock()
            .map(|mut w| std::mem::take(&mut *w))
            .unwrap_or_default()
    }
}

/// Read a whole content through a cursor, fetching each hole as it is reached.
///
/// Returns the plaintext and how many windows had to be fetched.
async fn page_through(
    seeker: &Node,
    holder: &Node,
    content: &ContentRef,
    session: u8,
    quota: u64,
) -> (Vec<u8>, usize) {
    use runtime::content_cursor::{Advance, ContentCursor};

    let space = space();
    let allow = |_: ContentAction<'_>| Ok(());
    let policy = seeker.policy(&space, &allow);
    let supply = Arc::new(Wanted::default());
    let provider = connect_provider(
        seeker.transport.as_ref(),
        &space,
        &seeker.station,
        &holder.station,
        [session; 16],
    )
    .await
    .expect("admitted");
    let providers = std::slice::from_ref(&provider);
    let mut fetcher = fetcher(seeker);
    fetcher.cache_quota_bytes = quota;
    let cancel = CancelToken::new();

    let mut cursor =
        ContentCursor::open(seeker.host.clone(), &policy, content, supply.clone()).expect("open");
    let mut out = Vec::new();
    let mut windows = 0usize;
    let mut operation = 0u8;
    loop {
        match cursor.next(&policy) {
            Advance::Yielded { cursor: next, span } => {
                out.extend_from_slice(span.bytes());
                cursor = next;
            }
            Advance::Blocked { cursor: next, .. } => {
                let chunks = supply.take();
                assert!(!chunks.is_empty(), "a hole names the chunk it is missing");
                operation = operation.wrapping_add(1);
                fetcher
                    .fetch_chunks(
                        content,
                        &chunks,
                        providers,
                        [session.wrapping_add(operation); 16],
                        Acquisition::Stream,
                        &cancel,
                        &anyone,
                    )
                    .await
                    .expect("the window arrives");
                windows += 1;
                cursor = next;
            }
            Advance::Finished { .. } => break,
            Advance::Refused(failure) => panic!("paging refused: {failure:?}"),
        }
    }
    (out, windows)
}

#[tokio::test(flavor = "current_thread")]
async fn a_content_larger_than_the_cache_pages_through_a_cursor_from_a_peer() {
    // The claim this whole line of work exists to make: file size stops
    // bounding playability, and only the window does. The seeker's quota is a
    // fraction of the content, so it cannot hold what it is reading.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "paging-holder", [21u8; 32], true);
    let seeker = node_with_quota(&net, "paging-seeker", [22u8; 32], false, 2 * 1024 * 1024);

    let plaintext = filler(9, 8 * 1024 * 1024);
    let content = seed_content(&holder, 9, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    // A quarter of what it is about to read.
    let quota = 2 * 1024 * 1024;
    let (read, windows) = page_through(&seeker, &holder, &content, 9, quota).await;

    assert_eq!(read.len(), plaintext.len(), "every byte arrived");
    assert!(read == plaintext, "and they are the bytes that were sealed");
    assert!(
        windows > 1,
        "a content this size cannot have arrived in one window under this quota"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn paging_holds_only_the_chunk_it_is_reading() {
    // The memory claim: one step is one chunk, whatever the content's size. If
    // the reader accumulated, a long film would be a long download wearing a
    // cursor's clothes.
    let net = comms::mem::MemNet::new();
    let holder = node(&net, "paging-bound-holder", [23u8; 32], true);
    let seeker = node_with_quota(&net, "paging-bound-seeker", [24u8; 32], false, 1024 * 1024);

    let plaintext = filler(11, 4 * 1024 * 1024);
    let content = seed_content(&holder, 11, &plaintext);
    learn_descriptor(&seeker, &holder, &content);

    let before = seeker.host.cache().resident_bytes();
    let (read, _) = page_through(&seeker, &holder, &content, 11, 1024 * 1024).await;
    assert_eq!(read.len(), plaintext.len());

    // Whatever is resident afterwards, it is bounded by the quota the fetch was
    // admitted against — not by the size of what was read.
    let after = seeker.host.cache().resident_bytes();
    assert!(
        after.saturating_sub(before) < plaintext.len() as u64,
        "a paged read left the whole content resident: {before} -> {after}"
    );
}
