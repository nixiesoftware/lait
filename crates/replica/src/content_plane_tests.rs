//! Plan 13 F2 exit — the content plane end to end, through the durable Replica.
//!
//! The property under test is the asymmetry that makes content worth having:
//! a descriptor is required material on every full Replica, and its bytes are
//! not. A peer can name a gigabyte it does not hold, and losing the bytes must
//! never look like a broken store.
//!
//! The other half is forgetting. A content catalog that can only grow means a
//! substrate that cannot forget an accidental upload, so reachability is
//! derived from what live Bodies declare — never a stored reference count,
//! which would not converge across independently committing replicas.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::body::{BodyBinding, Op, StaticBodyKeys, SupportedSchemas, MUTATION_ATOMIC};
use crate::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use crate::cache::Residency;
use crate::content::{
    open_resident_chunk, ContentDescriptor, ContentIngest, ContentRef, IngestedContent, Invalid,
    CHUNK_PLAINTEXT_LEN,
};
use crate::convergence::{AuthorityBatchReceipt, AuthorityIncorporator, StagedContactMaterial};
use crate::frontier::AuthorityFrontier;
use crate::manifest::ManifestRoot;
use crate::transaction::{CommitAuthorization, CommitContext, SeedSigner};
use crate::Replica;
use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;

const WRITER_SEED: [u8; 32] = [61u8; 32];
const EPOCH: [u8; 16] = [3u8; 16];
const EPOCH_KEY: [u8; 32] = [4u8; 32];
const OLD_EPOCH: [u8; 16] = [9u8; 16];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-plane-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

fn world() -> WorldId {
    WorldId::parse("com.example.notes").unwrap()
}

fn body(n: u8) -> BodyKey {
    BodyKey::new(world(), BodyId::from_bytes([n; 16]))
}

/// A second World in the same Space, for the declaring-world index.
///
/// It sorts before [`world`], so a test asserting the index's order is reading
/// the map's own ordering rather than the order the declarations happened in.
fn other_world() -> WorldId {
    WorldId::parse("com.example.gallery").unwrap()
}

fn other_body(n: u8) -> BodyKey {
    BodyKey::new(other_world(), BodyId::from_bytes([n; 16]))
}

fn key() -> AuthorizedBodyKey {
    AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY)
}

fn keys() -> Arc<StaticBodyKeys> {
    Arc::new(StaticBodyKeys::new(key()))
}

fn supported() -> SupportedSchemas {
    let mut s = SupportedSchemas::new();
    s.declare(
        world(),
        SchemaId::parse("blob").unwrap(),
        1,
        EncodingId::parse("bytes").unwrap(),
        MUTATION_ATOMIC,
    );
    s.declare(
        other_world(),
        SchemaId::parse("blob").unwrap(),
        1,
        EncodingId::parse("bytes").unwrap(),
        MUTATION_ATOMIC,
    );
    s
}

fn binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("blob").unwrap(),
        schema_version: 1,
        encoding: EncodingId::parse("bytes").unwrap(),
        mutation_model: MUTATION_ATOMIC,
    }
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

struct Fixture {
    replica: Replica,
    cache: Residency,
    dir: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let dir = temp_dir(tag);
    let mut replica = Replica::open(dir.join("store"), keys()).unwrap();
    replica.set_supported(supported());
    let cache = Residency::open(dir.join("cache"), 1 << 30).unwrap();
    Fixture {
        replica,
        cache,
        dir,
    }
}

fn ctx<'a>(signer: &'a SeedSigner<'a>, space: &'a SpaceId) -> CommitContext<'a> {
    CommitContext {
        space,
        signer,
        authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
    }
}

fn commit_body(replica: &mut Replica, seq: u8, key: &BodyKey, value: &[u8]) {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let context = ctx(&signer, &space);
    let mut request = [0u8; 16];
    request[0] = seq;
    replica
        .commit_action(
            &context,
            &CommitAuthorization {
                actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
                parent_manifest_root: [0u8; 32],
                demand: demand(),
                intent_digest: [7u8; 32],
                authorizer: &crate::transaction::StaticAuthorizer {
                    world: world(),
                    implementation_id: [0u8; 32],
                },
            },
            &world(),
            &mechanics::actor::device_from_seed(&WRITER_SEED),
            &request,
            &[7u8; 32],
            Vec::new(),
            Vec::new(),
            "plane",
            &[(
                key.clone(),
                Op::ReplaceAtomic {
                    value: value.to_vec(),
                },
            )],
            &[(key.clone(), binding())],
            &[],
        )
        .expect("commit body");
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

fn ingest(fx: &Fixture, operation: u8, plaintext: &[u8]) -> IngestedContent {
    let mut ingest = ContentIngest::begin(&space(), &key(), [operation; 16], &fx.cache, u64::MAX)
        .expect("begin ingest");
    for piece in plaintext.chunks(4096) {
        ingest.push(piece).unwrap();
    }
    if plaintext.is_empty() {
        ingest.push(&[]).unwrap();
    }
    ingest.finish().unwrap()
}

fn read_whole(fx: &Fixture, out: &IngestedContent) -> Vec<u8> {
    let mut bytes = Vec::new();
    for lease in &out.leases {
        bytes.extend_from_slice(
            &open_resident_chunk(&out.descriptor, &key(), &fx.cache, &lease.entry).unwrap(),
        );
    }
    bytes
}

#[test]
fn content_round_trips_at_every_boundary_and_commits_a_descriptor() {
    let mut fx = fixture("roundtrip");
    let chunk = CHUNK_PLAINTEXT_LEN as usize;
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);

    for (n, len) in [0usize, 1, chunk - 1, chunk, chunk + 1, chunk * 2 + 5]
        .into_iter()
        .enumerate()
    {
        let plaintext = filler(len as u64, len);
        let out = ingest(&fx, n as u8 + 1, &plaintext);
        assert!(
            read_whole(&fx, &out) == plaintext,
            "round trip failed at {len}"
        );

        let context = ctx(&signer, &space);
        let refs = fx
            .replica
            .commit_content(&context, std::slice::from_ref(&out.descriptor))
            .unwrap();
        assert_eq!(refs, vec![out.content_ref]);
        assert_eq!(
            fx.replica.content_descriptor(&out.content_ref),
            Some(out.descriptor.clone()),
            "a committed descriptor is readable back"
        );
    }
}

#[test]
fn a_descriptor_survives_reopen_while_its_bytes_need_not() {
    // The asymmetry the whole plane exists for.
    let fx = fixture("asymmetry");
    let plaintext = filler(1, CHUNK_PLAINTEXT_LEN as usize + 100);
    let out = ingest(&fx, 1, &plaintext);
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let mut replica = fx.replica;
    replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    drop(replica);

    // A peer that never fetched the bytes at all: a fresh, empty cache.
    let evicted = Residency::open(fx.dir.join("empty-cache"), 1 << 20).unwrap();

    // The store still opens, and still knows the content exists.
    let replica = Replica::open(fx.dir.join("store"), keys()).unwrap();
    let descriptor = replica
        .content_descriptor(&out.content_ref)
        .expect("the descriptor is required material and survives");
    assert_eq!(descriptor.plaintext_len, plaintext.len() as u64);
    assert_eq!(
        open_resident_chunk(&descriptor, &key(), &evicted, &out.leases[0].entry),
        Err(Invalid::NotResident),
        "and the bytes are honestly absent rather than an integrity failure"
    );
}

#[test]
fn a_corrupt_chunk_is_dropped_and_the_store_is_untouched() {
    let fx = fixture("corrupt");
    let plaintext = filler(2, 5_000);
    let out = ingest(&fx, 1, &plaintext);

    let path = fx
        .cache
        .root()
        .join("chunks")
        .join(data_encoding::HEXLOWER.encode(&out.leases[0].entry));
    std::fs::write(&path, b"tampered").unwrap();

    assert!(matches!(
        open_resident_chunk(&out.descriptor, &key(), &fx.cache, &out.leases[0].entry),
        Err(Invalid::ChunkMismatch | Invalid::NotResident)
    ));
    // Reopening the store proves the authoritative side never noticed.
    Replica::open(fx.dir.join("store"), keys())
        .expect("a bad cache line is not an integrity failure");
}

#[test]
fn a_provider_holding_a_proper_subset_still_serves_what_it_has() {
    // Residency is per chunk, so partial holdings are the normal case, not a
    // degraded one.
    let fx = fixture("subset");
    let plaintext = filler(3, CHUNK_PLAINTEXT_LEN as usize * 3);
    let out = ingest(&fx, 1, &plaintext);
    assert_eq!(out.leases.len(), 3);

    // Drop the middle chunk: both holds on it go, and only it is swept.
    let partial = Residency::open(fx.cache.root(), 0).unwrap();
    partial.release(&out.leases[1]).unwrap();
    partial
        .release(&crate::cache::Lease::content(
            out.descriptor.content_nonce,
            out.leases[1].entry,
        ))
        .unwrap();
    partial.sweep().unwrap();

    assert!(open_resident_chunk(&out.descriptor, &key(), &partial, &out.leases[0].entry).is_ok());
    assert_eq!(
        open_resident_chunk(&out.descriptor, &key(), &partial, &out.leases[1].entry),
        Err(Invalid::NotResident)
    );
    assert!(open_resident_chunk(&out.descriptor, &key(), &partial, &out.leases[2].entry).is_ok());
}

#[test]
fn losing_a_resident_chunk_costs_that_chunk_and_nothing_more() {
    // Resident bytes are reconstructable, not authoritative: losing one chunk
    // makes it unservable until it is refetched, and nothing else. The proof
    // cannot be lost separately — bytes and sidecar are one file, so there is
    // no state where a chunk is here and unservable.
    let fx = fixture("sidecar");
    let plaintext = filler(4, 3_000);
    let out = ingest(&fx, 1, &plaintext);
    let entry = out.leases[0].entry;

    std::fs::remove_file(
        fx.cache
            .root()
            .join("chunks")
            .join(data_encoding::HEXLOWER.encode(&entry)),
    )
    .unwrap();
    assert!(!fx.cache.is_resident(&entry), "not advertisable without it");
    assert_eq!(
        open_resident_chunk(&out.descriptor, &key(), &fx.cache, &entry),
        Err(Invalid::NotResident)
    );

    // Reconstructed by re-ingesting the same bytes: the descriptor is a pure
    // function of the sealed chunks, so a second ingest of the same plaintext
    // is a *different* content — which is why reconstruction means refetching
    // this one's chunk, not re-deriving it locally from the plaintext.
    let again = ingest(&fx, 2, &plaintext);
    assert_ne!(
        again.content_ref, out.content_ref,
        "no convergent encryption: identical bytes are not the same content"
    );
}

#[test]
fn content_sealed_under_an_old_epoch_stays_readable_under_that_epoch() {
    // Key rotation applies to future content. Existing immutable content is not
    // re-sealed, because re-sealing would change its id.
    let fx = fixture("epoch");
    let old_key = AuthorizedBodyKey::for_authorized_epoch(OLD_EPOCH, [7u8; 32]);
    let plaintext = filler(5, 2_000);
    let mut ingest = ContentIngest::begin(&space(), &old_key, [1u8; 16], &fx.cache, u64::MAX)
        .expect("begin ingest");
    ingest.push(&plaintext).unwrap();
    let out = ingest.finish().unwrap();

    assert_eq!(out.descriptor.epoch, OLD_EPOCH);
    assert_eq!(
        open_resident_chunk(&out.descriptor, &old_key, &fx.cache, &out.leases[0].entry).unwrap(),
        plaintext
    );
    // The current epoch's key opens nothing of it, and says so with one answer.
    assert_eq!(
        open_resident_chunk(&out.descriptor, &key(), &fx.cache, &out.leases[0].entry),
        Err(Invalid::Unopenable)
    );
}

#[test]
fn a_declaration_names_only_committed_content_and_a_held_body() {
    let mut fx = fixture("declare-guard");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    commit_body(&mut fx.replica, 1, &body(1), b"an issue");

    let phantom = ContentRef {
        content_id: [0xAB; 32],
    };
    let mut declarations = BTreeMap::new();
    declarations.insert(body(1), vec![phantom]);
    assert!(
        fx.replica
            .declare_content(&ctx(&signer, &space), declarations)
            .is_err(),
        "a declaration naming uncommitted content is refused"
    );

    let out = ingest(&fx, 2, b"real content");
    fx.replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    let mut declarations = BTreeMap::new();
    declarations.insert(body(200), vec![out.content_ref]);
    assert!(
        fx.replica
            .declare_content(&ctx(&signer, &space), declarations)
            .is_err(),
        "a declaration naming an unheld Body is refused"
    );
}

#[test]
fn unreferenced_content_is_swept_and_leaves_no_residue() {
    // The F2 exit: a descriptor that becomes unreferenced is gone from the
    // catalog and its residency is released.
    let mut fx = fixture("sweep");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);

    commit_body(&mut fx.replica, 1, &body(1), b"an issue with an attachment");
    let kept = ingest(&fx, 1, b"still referenced");
    let dropped = ingest(&fx, 2, b"about to be forgotten");
    fx.replica
        .commit_content(
            &ctx(&signer, &space),
            &[kept.descriptor.clone(), dropped.descriptor.clone()],
        )
        .unwrap();
    // The descriptors are committed, so the ingests hand their holds over to
    // the content-scoped ones.
    fx.cache.release_operation(&[1u8; 16]).unwrap();
    fx.cache.release_operation(&[2u8; 16]).unwrap();

    let mut declarations = BTreeMap::new();
    declarations.insert(body(1), vec![kept.content_ref, dropped.content_ref]);
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();
    assert_eq!(fx.replica.declared_content(&body(1)).len(), 2);

    // Nothing is unreferenced yet, so a sweep is a no-op.
    assert!(fx
        .replica
        .sweep_unreferenced_content(&ctx(&signer, &space), Some(&fx.cache))
        .unwrap()
        .is_empty());

    // The Body stops referencing one of them.
    let mut declarations = BTreeMap::new();
    declarations.insert(body(1), vec![kept.content_ref]);
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();

    let swept = fx
        .replica
        .sweep_unreferenced_content(&ctx(&signer, &space), Some(&fx.cache))
        .unwrap();
    assert_eq!(swept, vec![dropped.content_ref]);
    assert_eq!(fx.replica.content_descriptor(&dropped.content_ref), None);
    assert!(
        fx.replica.content_descriptor(&kept.content_ref).is_some(),
        "the still-referenced descriptor survives"
    );

    // And the residency behind it goes with a cache sweep.
    let cache = Residency::open(fx.cache.root(), 0).unwrap();
    cache.sweep().unwrap();
    assert!(!cache.is_resident(&dropped.leases[0].entry));

    // The catalog is genuinely smaller after a reopen — no tombstone residue.
    drop(fx.replica);
    let reopened = Replica::open(fx.dir.join("store"), keys()).unwrap();
    assert_eq!(reopened.content_descriptor(&dropped.content_ref), None);
    assert!(reopened.content_descriptor(&kept.content_ref).is_some());
    assert_eq!(reopened.declared_content(&body(1)).len(), 1);
}

#[test]
fn a_tombstoned_body_stops_holding_its_content() {
    // Reachability is over *live* Bodies. A tombstone is not a live Body, so
    // what it used to declare becomes collectable.
    let mut fx = fixture("tombstone");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    commit_body(&mut fx.replica, 1, &body(1), b"doomed");
    let out = ingest(&fx, 1, b"attached to a doomed issue");
    fx.replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    fx.cache.release_operation(&[1u8; 16]).unwrap();
    let mut declarations = BTreeMap::new();
    declarations.insert(body(1), vec![out.content_ref]);
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();
    assert!(fx
        .replica
        .sweep_unreferenced_content(&ctx(&signer, &space), Some(&fx.cache))
        .unwrap()
        .is_empty());

    // Forget the Body, then sweep: the content it held is now unreferenced.
    fx.replica.forget_declaration(&body(1));
    assert_eq!(
        fx.replica
            .sweep_unreferenced_content(&ctx(&signer, &space), Some(&fx.cache))
            .unwrap(),
        vec![out.content_ref]
    );
}

#[test]
fn a_descriptor_is_the_same_size_whatever_it_describes() {
    // What every full Replica carries per content, independent of its size.
    let fx = fixture("fixed");
    let small = ingest(&fx, 1, b"tiny");
    let large = ingest(&fx, 2, &filler(9, CHUNK_PLAINTEXT_LEN as usize * 4));
    // Not byte-identical — postcard varints mean a larger length and chunk
    // count cost a byte or two more — but bounded and tiny either way, which
    // is the property that matters: what a Replica carries per content does
    // not grow with the content.
    let small_len = small.descriptor.encode().len();
    let large_len = large.descriptor.encode().len();
    assert!(
        large_len - small_len <= 4,
        "a descriptor must not grow with what it describes: {small_len} vs {large_len}"
    );
    assert!(large_len < 128, "and stays tiny: {large_len} bytes");
    let _: &ContentDescriptor = &small.descriptor;
}

#[test]
fn the_content_paths_never_publish_two_roots_at_one_coordinate() {
    // Equivocation is one signer claiming two different states at one point,
    // and `ManifestBook` defines the point as `(signer, replica_frontier)`.
    // Every content path signs a *new* root — a different content index, a
    // different catalog — so if the coordinate did not move with it, an honest
    // Station that ingested a file and then declared it would flag itself the
    // moment a peer wired incorporation to the book.
    let mut fx = fixture("no-equivocation");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let context = ctx(&signer, &space);

    let mut seen: Vec<([u8; 32], crate::frontier::ReplicaFrontier, [u8; 32])> = Vec::new();
    let mut record = |replica: &Replica, label: &str| {
        let root = replica
            .published_manifest_root()
            .unwrap_or_else(|| panic!("{label}: no published root"));
        let (signer, frontier) = root.coordinate();
        let hash = root.root_hash();
        if let Some((_, _, prior)) = seen.iter().find(|(s, f, _)| *s == signer && *f == frontier) {
            assert_eq!(
                *prior, hash,
                "{label}: a second root at one coordinate is equivocation"
            );
        }
        seen.push((signer, frontier, hash));
    };

    let body = body(1);
    commit_body(&mut fx.replica, 1, &body, b"a body to hang content on");
    record(&fx.replica, "after the body");

    let out = ingest(&fx, 1, &filler(1, 5_000));
    fx.replica
        .commit_content(&context, std::slice::from_ref(&out.descriptor))
        .expect("commit content");
    record(&fx.replica, "after committing content");

    fx.replica
        .declare_content(
            &context,
            BTreeMap::from([(body.clone(), vec![out.content_ref])]),
        )
        .expect("declare");
    record(&fx.replica, "after declaring");

    fx.replica.forget_declaration(&body);
    fx.replica
        .sweep_unreferenced_content(&context, Some(&fx.cache))
        .expect("sweep");
    record(&fx.replica, "after sweeping");

    // Four distinct published states, four distinct coordinates.
    let mut coordinates: Vec<_> = seen.iter().map(|(s, f, _)| (*s, *f)).collect();
    coordinates.dedup();
    assert_eq!(coordinates.len(), 4, "{seen:?}");
}

// ---------------------------------------------------------------------------
// Descriptor convergence: the half of the plane that makes content openable by
// anyone but its author.
//
// A Body's manifest entry names content by id. An id is not enough to ask for
// bytes — asking needs the geometry, the epoch, and the Merkle root, and those
// live only in the descriptor. A Contact that carried declarations without
// descriptors converged an attachment as a name its recipients could see and
// never open. These tests are about the descriptor making the same crossing the
// declaration makes, in the same commit, under the same signature.
// ---------------------------------------------------------------------------

struct WriterAuthorized;
impl crate::transaction::AuthoritySource for WriterAuthorized {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        *signer
            == mechanics::actor::device_from_seed(&WRITER_SEED)
                .key_bytes()
                .unwrap()
    }
}

struct FixtureIncorporator;
impl AuthorityIncorporator for FixtureIncorporator {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<AuthorityBatchReceipt, crate::convergence::Failure> {
        Ok(AuthorityBatchReceipt {
            space: space(),
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

/// A bag of index nodes that remembers which ones were read.
///
/// Reading is the only way to learn which nodes a root reaches: the index is
/// content-addressed, so a node carries no label saying which root wants it.
struct ReadingBag {
    nodes: BTreeMap<[u8; 32], Vec<u8>>,
    read: std::cell::RefCell<std::collections::BTreeSet<[u8; 32]>>,
}

impl ReadingBag {
    fn new(nodes: &[Vec<u8>]) -> Self {
        Self {
            nodes: nodes
                .iter()
                .map(|bytes| (journal::object_content_hash(bytes), bytes.clone()))
                .collect(),
            read: std::cell::RefCell::new(std::collections::BTreeSet::new()),
        }
    }
}

impl crate::index::NodeSource for ReadingBag {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.read.borrow_mut().insert(*hash);
        self.nodes.get(hash).cloned()
    }
}

/// Everything one replica would serve a peer over Contact, staged as the
/// untrusted material the peer actually receives.
fn stage(fx: &Fixture) -> StagedContactMaterial {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let context = ctx(&signer, &space);
    let material = fx.replica.export_material().unwrap();
    let (root, nodes) = fx.replica.export_manifest(&context).unwrap();
    let mut authority_records = vec![b"mechanics-authority-record".to_vec()];
    let mut bodies = Vec::new();
    for (tx, payloads) in &material {
        authority_records.push(tx.encode());
        for (key, envelope) in payloads {
            bodies.push((tx.id(), key.clone(), envelope.clone()));
        }
    }
    StagedContactMaterial {
        authority_records,
        manifest_root_bytes: root,
        manifest_nodes: nodes,
        bodies,
    }
}

/// Commit a Body, ingest content, commit its descriptor, and declare the one
/// against the other — an attachment, as far as the substrate is concerned.
fn author_with_content(fx: &mut Fixture, seq: u8, plaintext: &[u8]) -> ContentRef {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    commit_body(&mut fx.replica, seq, &body(seq), b"an issue with a file");
    let out = ingest(fx, seq, plaintext);
    fx.replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    let mut declarations = BTreeMap::new();
    declarations.insert(body(seq), vec![out.content_ref]);
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();
    out.content_ref
}

fn incorporate(
    fx: &mut Fixture,
    staged: &StagedContactMaterial,
) -> Result<(), crate::transaction::commit::Failure> {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let context = ctx(&signer, &space);
    let mut incorporator = FixtureIncorporator;
    let bundle = fx
        .replica
        .validate_contact(staged, &WriterAuthorized, &mut incorporator)?;
    fx.replica
        .incorporate_bundle(&context, bundle, &WriterAuthorized)?;
    Ok(())
}

#[test]
fn a_descriptor_crosses_a_contact_with_the_declaration_that_names_it() {
    // The gate on everything downstream: after one Contact, the receiver can
    // say what shape this content is without ever having held its bytes, and
    // nothing local planted the answer.
    let mut author = fixture("converge-author");
    let mut peer = fixture("converge-peer");
    let plaintext = filler(11, CHUNK_PLAINTEXT_LEN as usize * 2 + 17);
    let content = author_with_content(&mut author, 1, &plaintext);

    assert!(
        peer.replica.content_descriptor(&content).is_none(),
        "the peer starts knowing nothing about this content"
    );
    incorporate(&mut peer, &stage(&author)).expect("the bundle validates and incorporates");

    let here = peer
        .replica
        .content_descriptor(&content)
        .expect("the descriptor crossed");
    assert_eq!(
        here,
        author.replica.content_descriptor(&content).unwrap(),
        "and it is the author's descriptor, byte for byte"
    );
    assert_eq!(here.plaintext_len, plaintext.len() as u64);
    assert_eq!(here.content_ref(), content, "it is self-identifying");
    assert_eq!(
        peer.replica.declared_content(&body(1)),
        vec![content],
        "the declaration that names it crossed in the same bundle"
    );
    // What did not cross: the bytes. That asymmetry is the whole plane.
    assert_eq!(
        peer.cache.staged_bytes(),
        0,
        "a descriptor is not its content"
    );
}

#[test]
fn an_advertisement_that_cannot_back_its_declarations_is_refused_whole() {
    // The completeness rule, and why it is a rule: adopting an advertised root
    // is atomic, so a declaration the receiver cannot resolve would become
    // permanent local state naming content nobody here can ever ask for. The
    // gap would surface only when someone finally tried to open it.
    //
    // Two ways an advertisement fails to back itself, and they fail at
    // different depths.
    let mut author = fixture("incomplete-author");
    author_with_content(&mut author, 3, b"a file that will not travel");
    let honest = stage(&author);
    let root = ManifestRoot::decode_canonical(&honest.manifest_root_bytes).unwrap();
    assert!(
        root.content_index_root.is_some(),
        "the honest advertisement carries a content catalog"
    );

    // 1. The catalog's nodes are withheld. Omission is the tamper actually
    //    available: the root is signed, so substituting a descriptor moves its
    //    index key, and rewriting the root needs the author's key. This is
    //    caught by index verification, before any declaration is read.
    let bag = ReadingBag::new(&honest.manifest_nodes);
    crate::index::stream(&bag, root.content_index_root, &mut |_| {}).unwrap();
    let catalog_nodes = bag.read.borrow().clone();
    assert!(
        !catalog_nodes.is_empty(),
        "the catalog has nodes to withhold"
    );
    let mut withheld = stage(&author);
    withheld
        .manifest_nodes
        .retain(|node| !catalog_nodes.contains(&journal::object_content_hash(node)));
    let mut peer = fixture("incomplete-peer");
    let refusal = incorporate(&mut peer, &withheld)
        .expect_err("an advertisement missing its own catalog is refused");
    assert_eq!(
        refusal,
        crate::transaction::commit::Failure::Illegitimate(
            crate::transaction::commit::Invalid::Index
        )
    );
    assert!(peer.replica.declared_content(&body(3)).is_empty());

    // 2. A correctly signed advertisement that declares content and carries no
    //    catalog at all. This is not hypothetical: it is exactly what a peer
    //    running the shape this commit replaces publishes — entries naming
    //    content ids under a root whose content index is empty. Everything
    //    verifies; the declarations still cannot be resolved, and the rule that
    //    catches it is the one about resolving declarations, not the one about
    //    valid indexes.
    let signer = SeedSigner(&WRITER_SEED);
    let blind = ManifestRoot::sign_with(
        &space(),
        root.replica_frontier,
        root.body_index_root,
        None,
        AuthorityFrontier::from_canonical_bytes(vec![9]),
        &signer,
    )
    .unwrap();
    let mut content_blind = stage(&author);
    content_blind.manifest_root_bytes = blind.encode();
    content_blind
        .manifest_nodes
        .retain(|node| !catalog_nodes.contains(&journal::object_content_hash(node)));
    let mut peer = fixture("blind-peer");
    let refusal = incorporate(&mut peer, &content_blind)
        .expect_err("a declaration with no descriptor behind it is refused");
    assert_eq!(
        refusal,
        crate::transaction::commit::Failure::Illegitimate(
            crate::transaction::commit::Invalid::UnbackedContent
        )
    );
    assert!(
        peer.replica.declared_content(&body(3)).is_empty(),
        "nothing partial is retained"
    );
}

#[test]
fn a_peer_that_already_holds_the_descriptor_needs_it_sent_only_once() {
    // Convergence is incremental. A comment on an issue with an attachment must
    // not require re-resolving the attachment against the wire, or every later
    // Contact pays for every earlier upload.
    let mut author = fixture("incremental-author");
    let mut peer = fixture("incremental-peer");
    let content = author_with_content(&mut author, 5, b"the file");
    incorporate(&mut peer, &stage(&author)).expect("first contact");
    assert!(peer.replica.content_descriptor(&content).is_some());

    commit_body(&mut author.replica, 6, &body(5), b"the issue, commented on");
    let second = stage(&author);
    let bundle = {
        let mut incorporator = FixtureIncorporator;
        peer.replica
            .validate_contact(&second, &WriterAuthorized, &mut incorporator)
            .expect("second contact validates")
    };
    assert_eq!(
        bundle.descriptor_count(),
        0,
        "nothing new to adopt — the peer already holds this descriptor"
    );
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    peer.replica
        .incorporate_bundle(&ctx(&signer, &space), bundle, &WriterAuthorized)
        .expect("second contact incorporates");
    assert!(
        peer.replica.content_descriptor(&content).is_some(),
        "and the descriptor is still here"
    );
    assert_eq!(peer.replica.declared_content(&body(5)), vec![content]);
}

#[test]
fn content_a_body_stopped_declaring_is_not_pushed_at_a_peer() {
    // An advertisement carries what live Bodies reach, not everything on disk.
    // A descriptor awaiting this Station's own sweep is this Station's garbage;
    // exporting it would grow the peer's catalog from ours, and their sweep
    // would then have to undo it.
    let mut author = fixture("orphan-author");
    let mut peer = fixture("orphan-peer");
    let content = author_with_content(&mut author, 7, b"a file about to be orphaned");

    author.replica.forget_declaration(&body(7));
    assert!(
        author.replica.content_descriptor(&content).is_some(),
        "the author still holds it — the sweep is a separate beat"
    );

    incorporate(&mut peer, &stage(&author)).expect("contact");
    assert!(
        peer.replica.content_descriptor(&content).is_none(),
        "an unreachable descriptor is not advertised"
    );
}

// ---------------------------------------------------------------------------
// The upload-to-attach window.
//
// Reachability is derived from live Bodies, and between committing a descriptor
// and committing the Body that names it there is no such Body. For the whole of
// that window the content is garbage by the only rule the sweep has — and the
// sweep is right, because nothing on disk distinguishes an upload awaiting an
// attach from an upload nobody ever attached. A hold is what distinguishes
// them, and it lapses so the second case still ends.
// ---------------------------------------------------------------------------

#[test]
fn content_awaiting_its_body_survives_a_sweep_that_runs_first() {
    // Hostile ordering deliberately: upload, sweep, then attach. The sweep runs
    // on its own beat, so it can land anywhere, and "anywhere" includes the
    // worst moment.
    let mut fx = fixture("pending-hold");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    commit_body(
        &mut fx.replica,
        1,
        &body(1),
        b"an issue, not yet carrying a file",
    );

    let out = ingest(&fx, 2, b"bytes waiting for somewhere to belong");
    fx.replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    fx.replica.hold_content(
        &out.content_ref,
        std::time::Instant::now() + std::time::Duration::from_secs(600),
    );

    let collected = fx
        .replica
        .sweep_unreferenced_content(&ctx(&signer, &space), Some(&fx.cache))
        .unwrap();
    assert!(
        collected.is_empty(),
        "a held upload is not garbage yet: {collected:?}"
    );

    // And the attach still lands — which it could not if the descriptor were
    // gone, because a declaration must name committed content.
    let mut declarations = BTreeMap::new();
    declarations.insert(body(1), vec![out.content_ref]);
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .expect("the attach lands on content the sweep left alone");
    assert_eq!(fx.replica.declared_content(&body(1)), vec![out.content_ref]);
    assert_eq!(
        read_whole(&fx, &out),
        b"bytes waiting for somewhere to belong"
    );
}

#[test]
fn an_upload_nobody_attaches_is_collected_when_its_hold_lapses() {
    // The other half, and the reason the hold has a deadline at all: a window
    // that never closes is a leak with a comment on it.
    let mut fx = fixture("pending-lapse");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);

    let out = ingest(&fx, 3, b"an upload that was thought better of");
    fx.replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    fx.replica
        .hold_content(&out.content_ref, std::time::Instant::now());

    let collected = fx
        .replica
        .sweep_unreferenced_content(&ctx(&signer, &space), Some(&fx.cache))
        .unwrap();
    assert_eq!(
        collected,
        vec![out.content_ref],
        "the lapsed hold held nothing"
    );
    assert!(fx.replica.content_descriptor(&out.content_ref).is_none());
}

#[test]
fn a_hold_survives_its_deadline_exactly_and_not_a_moment_after() {
    // The two tests above check the two states — a long hold is kept, an
    // already-lapsed one is collected. Neither watches the SAME hold cross its
    // own deadline, which is where the comparison actually lives:
    // `retain(|_, until| *until > now)`. Whether that is `>` or `>=` decides
    // what happens to a sweep landing on the exact instant a hold expires, and
    // nothing distinguished the two.
    //
    // Reachable now because `sweep_unreferenced_content_at` takes the instant
    // rather than minting it. Waiting for a real deadline to pass would make
    // this a slow test with a race in it; supplying the instant makes it
    // neither.
    let mut fx = fixture("pending-boundary");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);

    let out = ingest(&fx, 7, b"an upload watched across its own deadline");
    fx.replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    fx.replica.hold_content(&out.content_ref, deadline);

    // One tick before: held.
    let collected = fx
        .replica
        .sweep_unreferenced_content_at(
            &ctx(&signer, &space),
            Some(&fx.cache),
            deadline - std::time::Duration::from_nanos(1),
        )
        .unwrap();
    assert!(collected.is_empty(), "a hold is live up to its deadline");

    // Exactly at the deadline: `until > now` is false, so the hold is over.
    // Asserted rather than assumed — a hold that outlived its own stated
    // deadline would be a window that never closes, which is the leak the
    // deadline exists to prevent.
    let collected = fx
        .replica
        .sweep_unreferenced_content_at(&ctx(&signer, &space), Some(&fx.cache), deadline)
        .unwrap();
    assert_eq!(
        collected,
        vec![out.content_ref],
        "the deadline is the moment the hold stops holding"
    );
    assert!(fx.replica.content_descriptor(&out.content_ref).is_none());
}

#[test]
fn a_held_descriptor_is_kept_here_and_shown_to_nobody() {
    // A hold answers "may I delete this", not "may I show this to a peer".
    // Advertising a descriptor no Body names would hand the peer catalog it has
    // no reason to keep, and their own sweep would have to undo it.
    let mut author = fixture("held-author");
    let mut peer = fixture("held-peer");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    commit_body(
        &mut author.replica,
        4,
        &body(4),
        b"an issue with nothing attached",
    );

    let out = ingest(&author, 5, b"held, undeclared");
    author
        .replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    author.replica.hold_content(
        &out.content_ref,
        std::time::Instant::now() + std::time::Duration::from_secs(600),
    );

    incorporate(&mut peer, &stage(&author)).expect("contact");
    assert!(
        peer.replica.content_descriptor(&out.content_ref).is_none(),
        "the peer was told about a Body, and nothing about a pending upload"
    );
    assert!(
        author
            .replica
            .content_descriptor(&out.content_ref)
            .is_some(),
        "and the author still has it"
    );
}

#[test]
fn abandoning_a_hold_makes_its_content_collectable_at_once() {
    // A caller that knows the upload is dead should not have to wait out a
    // deadline meant for a caller that does not know.
    let mut fx = fixture("hold-abandon");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);

    let out = ingest(&fx, 6, b"cancelled halfway through deciding");
    fx.replica
        .commit_content(&ctx(&signer, &space), std::slice::from_ref(&out.descriptor))
        .unwrap();
    fx.replica.hold_content(
        &out.content_ref,
        std::time::Instant::now() + std::time::Duration::from_secs(600),
    );
    fx.replica.release_content_hold(&out.content_ref);

    assert_eq!(
        fx.replica
            .sweep_unreferenced_content(&ctx(&signer, &space), Some(&fx.cache))
            .unwrap(),
        vec![out.content_ref]
    );
}

#[test]
fn the_declaring_world_index_names_who_the_bytes_belong_to_and_forgets_exactly() {
    // Serving a content is meant to become a decision about what the bytes
    // belong to, not only about who is asking. A ContentId is a hash with no
    // hierarchy and no equality oracle, so the content plane carries no
    // resource to scope that decision against — the Bodies declaring it do,
    // and each one names its World.
    //
    // The subtle half is forgetting. One World may declare one descriptor from
    // several Bodies, so dropping *a* declaration must not drop the World, and
    // dropping the last one must.
    let mut fx = fixture("declaring-worlds");
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);

    commit_body(&mut fx.replica, 1, &body(1), b"notes, one");
    commit_body(&mut fx.replica, 2, &body(2), b"notes, two");
    commit_body(&mut fx.replica, 3, &other_body(3), b"gallery, one");

    let shared = ingest(&fx, 1, b"bytes two Worlds both point at");
    fx.replica
        .commit_content(&ctx(&signer, &space), &[shared.descriptor.clone()])
        .unwrap();
    fx.cache.release_operation(&[1u8; 16]).unwrap();

    assert!(
        fx.replica.declaring_worlds(&shared.content_ref).is_empty(),
        "committed but undeclared bytes belong to nobody yet"
    );

    // Two Bodies of one World, and one of another.
    let mut declarations = BTreeMap::new();
    declarations.insert(body(1), vec![shared.content_ref]);
    declarations.insert(body(2), vec![shared.content_ref]);
    declarations.insert(other_body(3), vec![shared.content_ref]);
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();

    assert_eq!(
        fx.replica.declaring_worlds(&shared.content_ref),
        vec![other_world(), world()],
        "both Worlds, once each, in the index's own order"
    );

    // One of the two notes Bodies stops referencing it. The World does not
    // leave, because its other Body still declares the same bytes.
    let mut declarations = BTreeMap::new();
    declarations.insert(body(1), Vec::new());
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();
    assert_eq!(
        fx.replica.declaring_worlds(&shared.content_ref),
        vec![other_world(), world()],
        "a World with one declaration left still declares"
    );

    // The last one does leave.
    let mut declarations = BTreeMap::new();
    declarations.insert(body(2), Vec::new());
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();
    assert_eq!(
        fx.replica.declaring_worlds(&shared.content_ref),
        vec![other_world()],
        "the World goes when its last declaration does"
    );

    // And when nothing declares them, the bytes belong to nobody again —
    // the same answer the reachability sweep acts on.
    let mut declarations = BTreeMap::new();
    declarations.insert(other_body(3), Vec::new());
    fx.replica
        .declare_content(&ctx(&signer, &space), declarations)
        .unwrap();
    assert!(
        fx.replica.declaring_worlds(&shared.content_ref).is_empty(),
        "no live Body names these bytes, so no World does either"
    );
}
