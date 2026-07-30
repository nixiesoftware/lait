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

use fabric::journal::cache::ResidentCache;
use mechanics::crypto::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::content::{
    open_resident_chunk, ContentDescriptor, ContentError, ContentIngest, ContentRef,
    IngestedContent, CHUNK_PLAINTEXT_LEN,
};
use replica::frontier::AuthorityFrontier;
use replica::{
    BodyBinding, BodyId, BodyKey, BodyOp, CommitAuthorization, CommitContext, EncodingId, Replica,
    SchemaId, SeedSigner, StaticBodyKeys, SupportedSchemas, WorldId, MUTATION_ATOMIC,
};

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
    use mechanics::demand::{AuthorizationDemand, PolicyCapability, PolicyResource};
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.notes", "write"),
        PolicyResource::space("com.example.notes"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

struct Fixture {
    replica: Replica,
    cache: ResidentCache,
    dir: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let dir = temp_dir(tag);
    let mut replica = Replica::open_journaled(dir.join("store"), keys()).unwrap();
    replica.set_supported(supported());
    let cache = ResidentCache::open(dir.join("cache"), 1 << 30).unwrap();
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
                actor: "plane",
                parent_manifest_root: [0u8; 32],
                demand: demand(),
                intent_digest: [7u8; 32],
                authorizer: &replica::StaticAuthorizer {
                    world: world(),
                    implementation_id: [0u8; 32],
                },
            },
            &world(),
            &mechanics::crypto::device_from_seed(&WRITER_SEED),
            &request,
            &[7u8; 32],
            Vec::new(),
            Vec::new(),
            "plane",
            &[(
                key.clone(),
                BodyOp::ReplaceAtomic {
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
    let mut ingest = ContentIngest::begin(&space(), &key(), [operation; 16], &fx.cache, u64::MAX);
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
    let evicted = ResidentCache::open(fx.dir.join("empty-cache"), 1 << 20).unwrap();

    // The store still opens, and still knows the content exists.
    let replica = Replica::open_journaled(fx.dir.join("store"), keys()).unwrap();
    let descriptor = replica
        .content_descriptor(&out.content_ref)
        .expect("the descriptor is required material and survives");
    assert_eq!(descriptor.plaintext_len, plaintext.len() as u64);
    assert_eq!(
        open_resident_chunk(&descriptor, &key(), &evicted, &out.leases[0].entry),
        Err(ContentError::NotResident),
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
        Err(ContentError::ChunkMismatch | ContentError::NotResident)
    ));
    // Reopening the store proves the authoritative side never noticed.
    Replica::open_journaled(fx.dir.join("store"), keys())
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
    let partial = ResidentCache::open(fx.cache.root(), 0).unwrap();
    partial.release(&out.leases[1]).unwrap();
    partial
        .release(&fabric::journal::cache::Lease::content(
            out.descriptor.content_nonce,
            out.leases[1].entry,
        ))
        .unwrap();
    partial.sweep().unwrap();

    assert!(open_resident_chunk(&out.descriptor, &key(), &partial, &out.leases[0].entry).is_ok());
    assert_eq!(
        open_resident_chunk(&out.descriptor, &key(), &partial, &out.leases[1].entry),
        Err(ContentError::NotResident)
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
        Err(ContentError::NotResident)
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
    let mut ingest = ContentIngest::begin(&space(), &old_key, [1u8; 16], &fx.cache, u64::MAX);
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
        Err(ContentError::Unopenable)
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
    let cache = ResidentCache::open(fx.cache.root(), 0).unwrap();
    cache.sweep().unwrap();
    assert!(!cache.is_resident(&dropped.leases[0].entry));

    // The catalog is genuinely smaller after a reopen — no tombstone residue.
    drop(fx.replica);
    let reopened = Replica::open_journaled(fx.dir.join("store"), keys()).unwrap();
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

    let mut seen: Vec<([u8; 32], replica::frontier::ReplicaFrontier, [u8; 32])> = Vec::new();
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
