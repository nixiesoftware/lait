//! Manifest fixtures: canonical roundtrip, signature binding, index
//! substitution and omission, entry misplacement, count lies, valid concurrent
//! roots coexisting, replay dedup, same-coordinate equivocation, and the
//! retention bound on the book that detects it.

use std::collections::BTreeMap;

use fabric::journal::index::{node_hash, ChildRef, IndexNode, NodeSink, NodeSource};
use mechanics::ids::SpaceId;
use replica::frontier::AuthorityFrontier as AF;
use replica::frontier::{AuthorityFrontier, ReplicaFrontier};
use replica::ids::{BodyId, BodyKey, WorldId};
use replica::manifest::{
    build_body_index, ManifestBook, ManifestEntry, ManifestError, ManifestHead, ManifestRoot,
    RootObservation,
};
use replica::transaction::{AuthoritySource, SeedSigner};

const SIGNER_SEED: [u8; 32] = [81u8; 32];
const OTHER_SEED: [u8; 32] = [82u8; 32];

/// A mechanics view that authorizes both test signer seeds.
struct BothSigners;
impl AuthoritySource for BothSigners {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AF) -> bool {
        let s1 = mechanics::crypto::device_from_seed(&SIGNER_SEED)
            .key_bytes()
            .unwrap();
        let s2 = mechanics::crypto::device_from_seed(&OTHER_SEED)
            .key_bytes()
            .unwrap();
        *signer == s1 || *signer == s2
    }
}

/// An in-memory node store, standing in for the object directory a real
/// verifier reads from.
#[derive(Default)]
struct Nodes(BTreeMap<[u8; 32], Vec<u8>>);

impl Nodes {
    fn absorb(&mut self, sink: NodeSink) {
        for bytes in sink.written {
            self.0.insert(node_hash(&bytes), bytes);
        }
    }
}

impl NodeSource for Nodes {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.0.get(hash).cloned()
    }
}

fn space() -> SpaceId {
    SpaceId::from_digest([12u8; 16])
}

fn frontier(n: u64) -> ReplicaFrontier {
    ReplicaFrontier::new([n as u8; 32], n)
}

fn auth() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![1])
}

fn body(n: u8) -> BodyKey {
    BodyKey::new(
        WorldId::parse("com.example.notes").unwrap(),
        BodyId::from_bytes([n; 16]),
    )
}

fn entry(n: u8) -> ManifestEntry {
    ManifestEntry::new(
        body(n),
        vec![ManifestHead {
            descriptor_hash: [n; 32],
            transaction_commitment: [n; 32],
        }],
    )
    .unwrap()
}

/// Sign a root over the given entries, with the nodes that prove it.
fn signed(entries: Vec<ManifestEntry>, at: u64, seed: &[u8; 32]) -> (ManifestRoot, Nodes) {
    let mut nodes = Nodes::default();
    let mut sink = NodeSink::default();
    let body_root = build_body_index(entries, &mut sink).unwrap();
    nodes.absorb(sink);
    let root = ManifestRoot::sign_with(
        &space(),
        frontier(at),
        body_root,
        None,
        auth(),
        &SeedSigner(seed),
    )
    .unwrap();
    (root, nodes)
}

fn valid_manifest() -> (ManifestRoot, Nodes) {
    signed(vec![entry(1), entry(2), entry(3)], 1, &SIGNER_SEED)
}

#[test]
fn a_valid_manifest_roundtrips_and_verifies() {
    let (root, nodes) = valid_manifest();
    let encoded = root.encode();
    assert_eq!(ManifestRoot::decode_canonical(&encoded).unwrap(), root);
    root.verify().unwrap();
    assert_eq!(root.verify_index(&nodes).unwrap(), 3);
    assert_eq!(root.body_count, 3);
}

#[test]
fn a_non_canonical_encoding_is_refused() {
    let (root, _) = valid_manifest();
    let mut extended = root.encode();
    extended.push(0);
    assert_eq!(
        ManifestRoot::decode_canonical(&extended),
        Err(ManifestError::NonCanonical)
    );
}

#[test]
fn the_signature_binds_every_field_it_covers() {
    let (root, _) = valid_manifest();
    let mutations: [fn(&mut ManifestRoot); 5] = [
        |r| r.replica_frontier = frontier(9),
        |r| r.body_index_root = None,
        |r| r.content_count += 1,
        |r| r.signer = [0u8; 32],
        |r| r.authority_frontier = AuthorityFrontier::from_canonical_bytes(vec![9]),
    ];
    for mutate in mutations {
        let mut tampered = root.clone();
        mutate(&mut tampered);
        assert!(tampered.verify().is_err(), "a mutated root must not verify");
    }
}

#[test]
fn a_count_that_disagrees_with_the_index_is_refused_before_the_signature() {
    // Cheap checks come first, so a malformed root costs no elliptic curve.
    let (mut root, _) = valid_manifest();
    root.body_count = 99;
    assert_eq!(root.verify(), Err(ManifestError::CountMismatch));
}

#[test]
fn a_substituted_entry_is_caught_by_its_placement() {
    // Index validation proves an entry sits under *some* key. Only re-deriving
    // the key from the entry's own BodyKey proves it sits under *its* key —
    // otherwise one Body's advertised heads could be served for another.
    let (root, mut nodes) = signed(vec![entry(1)], 1, &SIGNER_SEED);
    let leaf_hash = root.body_index_root.unwrap().hash;
    let leaf = nodes.node(&leaf_hash).unwrap();
    let mut decoded = IndexNode::decode_canonical(&leaf).unwrap();
    if let IndexNode::Leaf(entries) = &mut decoded {
        // Same index key, a different Body's entry inside it.
        entries[0].value = entry(2).encode();
    }
    let swapped = decoded.encode();
    nodes.0.insert(node_hash(&swapped), swapped.clone());
    let tampered = ManifestRoot {
        body_index_root: Some(ChildRef {
            hash: node_hash(&swapped),
            count: 1,
        }),
        ..root
    };
    assert_eq!(
        tampered.verify_index(&nodes),
        Err(ManifestError::KeyMismatch)
    );
}

#[test]
fn a_missing_node_fails_verification_rather_than_shrinking_the_catalog() {
    let (root, _) = valid_manifest();
    assert_eq!(
        root.verify_index(&Nodes::default()),
        Err(ManifestError::IndexInvalid)
    );
}

#[test]
fn a_body_with_no_heads_is_not_an_entry() {
    assert_eq!(
        ManifestEntry::new(body(1), Vec::new()),
        Err(ManifestError::Bounds)
    );
}

#[test]
fn heads_are_a_canonical_set() {
    // Two replicas holding the same Body must publish the same bytes for it,
    // whatever order their heads arrived in.
    let a = ManifestHead {
        descriptor_hash: [1u8; 32],
        transaction_commitment: [1u8; 32],
    };
    let b = ManifestHead {
        descriptor_hash: [2u8; 32],
        transaction_commitment: [2u8; 32],
    };
    let forward = ManifestEntry::new(body(1), vec![a, b]).unwrap();
    let reverse = ManifestEntry::new(body(1), vec![b, a]).unwrap();
    let duplicated = ManifestEntry::new(body(1), vec![a, b, a]).unwrap();
    assert_eq!(forward.encode(), reverse.encode());
    assert_eq!(forward.encode(), duplicated.encode());
}

#[test]
fn an_unsorted_entry_from_the_wire_is_refused() {
    let a = ManifestHead {
        descriptor_hash: [1u8; 32],
        transaction_commitment: [1u8; 32],
    };
    let b = ManifestHead {
        descriptor_hash: [2u8; 32],
        transaction_commitment: [2u8; 32],
    };
    let bad = ManifestEntry {
        key: body(1),
        heads: vec![b, a],
    };
    assert_eq!(
        ManifestEntry::decode_canonical(&postcard::to_stdvec(&bad).unwrap()),
        Err(ManifestError::OrderViolation)
    );
}

#[test]
fn one_catalog_has_one_root_however_it_was_assembled() {
    // The property the index exists for, at the manifest layer: two replicas
    // holding the same Bodies publish the same root.
    let (forward, _) = signed(vec![entry(1), entry(2), entry(3)], 1, &SIGNER_SEED);
    let (reverse, _) = signed(vec![entry(3), entry(1), entry(2)], 1, &SIGNER_SEED);
    assert_eq!(forward.body_index_root, reverse.body_index_root);
    assert_eq!(forward.encode(), reverse.encode());
}

#[test]
fn concurrent_roots_coexist_and_replays_dedupe() {
    let mut book = ManifestBook::new();
    let (mine, _) = signed(vec![entry(1)], 1, &SIGNER_SEED);
    let (theirs, _) = signed(vec![entry(2)], 2, &OTHER_SEED);

    let mine = mine.verify_authorized(&BothSigners).unwrap();
    let theirs = theirs.verify_authorized(&BothSigners).unwrap();
    assert_eq!(book.observe(&mine).unwrap(), RootObservation::Accepted);
    assert_eq!(book.observe(&theirs).unwrap(), RootObservation::Accepted);
    assert_eq!(book.observe(&mine).unwrap(), RootObservation::AlreadyKnown);
    assert_eq!(book.len(), 2);
}

#[test]
fn two_different_roots_at_one_coordinate_are_equivocation() {
    let mut book = ManifestBook::new();
    let (first, _) = signed(vec![entry(1)], 1, &SIGNER_SEED);
    let (second, _) = signed(vec![entry(2)], 1, &SIGNER_SEED);
    assert_eq!(first.coordinate(), second.coordinate());

    book.observe(&first.verify_authorized(&BothSigners).unwrap())
        .unwrap();
    assert_eq!(
        book.observe(&second.verify_authorized(&BothSigners).unwrap()),
        Err(ManifestError::Equivocation)
    );
}

#[test]
fn an_unauthorized_signer_never_reaches_the_book() {
    struct NobodyAuthorized;
    impl AuthoritySource for NobodyAuthorized {
        fn signer_authorized(&self, _s: &[u8; 32], _f: &AF) -> bool {
            false
        }
    }
    let (root, _) = valid_manifest();
    assert_eq!(
        root.verify_authorized(&NobodyAuthorized).unwrap_err(),
        ManifestError::AuthorityUnverified
    );
}

#[test]
fn the_book_is_bounded_and_says_so_when_it_forgets() {
    // Unbounded retention was remote-driven growth. Bounded retention means a
    // forgotten coordinate is one a signer could later equivocate at
    // undetected, so eviction is reported rather than silent.
    let mut book = ManifestBook::with_limit(4);
    let mut evictions = 0;
    for n in 1..=10u64 {
        let (root, _) = signed(vec![entry(n as u8)], n, &SIGNER_SEED);
        if let RootObservation::AcceptedWithEviction { evicted } = book
            .observe(&root.verify_authorized(&BothSigners).unwrap())
            .unwrap()
        {
            evictions += evicted;
        }
    }
    assert_eq!(book.len(), 4, "retention holds the limit");
    assert_eq!(evictions, 6, "and every drop was reported");
}

#[test]
fn one_signer_cannot_evict_another() {
    // Per-signer retention: a noisy peer must not be able to flush the history
    // that would convict a quiet one.
    let mut book = ManifestBook::with_limit(2);
    let (quiet, _) = signed(vec![entry(1)], 1, &OTHER_SEED);
    book.observe(&quiet.verify_authorized(&BothSigners).unwrap())
        .unwrap();
    for n in 1..=20u64 {
        let (noisy, _) = signed(vec![entry(n as u8)], n, &SIGNER_SEED);
        book.observe(&noisy.verify_authorized(&BothSigners).unwrap())
            .unwrap();
    }
    let (forged, _) = signed(vec![entry(9)], 1, &OTHER_SEED);
    assert_eq!(
        book.observe(&forged.verify_authorized(&BothSigners).unwrap()),
        Err(ManifestError::Equivocation)
    );
}

#[test]
fn a_zero_limit_still_retains_something() {
    // A book that retains nothing cannot detect an equivocation at all, so the
    // operator knob floors at one rather than accepting a useless setting.
    let mut book = ManifestBook::with_limit(0);
    let (root, _) = signed(vec![entry(1)], 1, &SIGNER_SEED);
    book.observe(&root.verify_authorized(&BothSigners).unwrap())
        .unwrap();
    let (other, _) = signed(vec![entry(2)], 1, &SIGNER_SEED);
    assert_eq!(
        book.observe(&other.verify_authorized(&BothSigners).unwrap()),
        Err(ManifestError::Equivocation)
    );
}
