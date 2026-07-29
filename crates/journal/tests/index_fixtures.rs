//! Plan 13 F1 item 1 — the radix index core, proven before anything uses it.
//!
//! Four crates end up depending on this one mechanism, so its properties are
//! worth stating as tests rather than as comments. The load-bearing ones:
//!
//! - **canonical**: one entry set, one encoding, regardless of the edits that
//!   produced it. A root hash that depended on edit order would commit to a
//!   history rather than a set, and two peers holding identical state would
//!   publish different roots.
//! - **O(changed)**: an update writes nodes proportional to what changed times
//!   the depth, not to the size of the set.
//! - **validated**: a structurally consistent but non-canonical tree is
//!   refused, or a set would have more than one legal root.

use std::collections::BTreeMap;

use journal::index::{
    apply, build_index, lookup, node_hash, spine, stream, validate, ChildRef, IndexChange,
    IndexEntry, IndexError, IndexKey, NodeSink, NodeSource, MAX_LEAF_ENTRIES, MAX_VALUE_BYTES,
};

/// An in-memory node store. Nodes are never deleted here, so a test can assert
/// that an update did not *read* a subtree as easily as that it did not write
/// one.
#[derive(Default)]
struct Nodes {
    map: BTreeMap<[u8; 32], Vec<u8>>,
}

impl Nodes {
    fn absorb(&mut self, sink: NodeSink) {
        for bytes in sink.written {
            self.map.insert(node_hash(&bytes), bytes);
        }
    }
}

impl NodeSource for Nodes {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.map.get(hash).cloned()
    }
}

/// Deterministic key from an ordinal, hashed so the tree sees the same
/// distribution real callers give it.
fn key(n: u64) -> IndexKey {
    *blake3::hash(&n.to_be_bytes()).as_bytes()
}

fn entry(n: u64, value: &[u8]) -> IndexEntry {
    IndexEntry {
        key: key(n),
        value: value.to_vec(),
    }
}

fn set(n: u64, value: &[u8]) -> IndexChange {
    IndexChange {
        key: key(n),
        value: Some(value.to_vec()),
    }
}

fn remove(n: u64) -> IndexChange {
    IndexChange {
        key: key(n),
        value: None,
    }
}

/// Build an index of `count` entries from scratch.
fn built(count: u64) -> (Nodes, Option<ChildRef>) {
    let mut nodes = Nodes::default();
    let mut sink = NodeSink::default();
    let root = build_index(
        (0..count).map(|n| entry(n, &n.to_be_bytes())).collect(),
        &mut sink,
    )
    .expect("build");
    nodes.absorb(sink);
    (nodes, root)
}

/// Build the same index by applying one change at a time.
fn grown(count: u64) -> (Nodes, Option<ChildRef>) {
    let mut nodes = Nodes::default();
    let mut root = None;
    for n in 0..count {
        let mut sink = NodeSink::default();
        root = apply(&nodes, root, vec![set(n, &n.to_be_bytes())], &mut sink).expect("apply");
        nodes.absorb(sink);
    }
    (nodes, root)
}

#[test]
fn one_entry_set_has_one_encoding_however_it_was_reached() {
    // The canonical property. Insert order, batch size, and whether the tree
    // was built at once or grown a key at a time must all be invisible in the
    // root.
    for count in [1u64, 2, 17, MAX_LEAF_ENTRIES as u64, 257, 1_000, 5_000] {
        let (_, from_scratch) = built(count);
        let (_, incremental) = grown(count);
        assert_eq!(
            from_scratch, incremental,
            "{count} entries produced different roots depending on how they arrived"
        );

        // And in reverse insertion order.
        let mut nodes = Nodes::default();
        let mut root = None;
        for n in (0..count).rev() {
            let mut sink = NodeSink::default();
            root = apply(&nodes, root, vec![set(n, &n.to_be_bytes())], &mut sink).expect("apply");
            nodes.absorb(sink);
        }
        assert_eq!(root, from_scratch, "reverse insertion changed the root");
    }
}

#[test]
fn a_removed_key_leaves_the_root_it_started_from() {
    // Add then remove must return to the exact prior root, or the structure is
    // accumulating history it claims not to hold.
    let (mut nodes, root) = built(1_000);
    let mut sink = NodeSink::default();
    let added = apply(&nodes, root, vec![set(9_999, b"transient")], &mut sink).expect("add");
    nodes.absorb(sink);
    assert_ne!(added, root);

    let mut sink = NodeSink::default();
    let removed = apply(&nodes, added, vec![remove(9_999)], &mut sink).expect("remove");
    nodes.absorb(sink);
    assert_eq!(removed, root, "add-then-remove must restore the prior root");
}

#[test]
fn removing_everything_yields_an_empty_root() {
    let (mut nodes, root) = built(500);
    let mut sink = NodeSink::default();
    let emptied =
        apply(&nodes, root, (0..500).map(remove).collect(), &mut sink).expect("remove all");
    nodes.absorb(sink);
    assert_eq!(emptied, None);
    assert_eq!(validate(&nodes, emptied), Ok(0));
}

#[test]
fn an_update_writes_nodes_proportional_to_depth_not_to_size() {
    // The reason this module exists. If this ever regresses, the commit-cost
    // fix has silently un-fixed itself.
    let mut written_at = Vec::new();
    for count in [1_000u64, 10_000, 50_000] {
        let (nodes, root) = built(count);
        let mut sink = NodeSink::default();
        apply(&nodes, root, vec![set(0, b"changed")], &mut sink).expect("apply");
        written_at.push((count, sink.written.len()));
    }
    for (count, written) in &written_at {
        assert!(
            *written <= 8,
            "a one-key change at {count} entries wrote {written} nodes — \
             the whole point is that this does not grow with the set"
        );
    }
    let smallest = written_at.first().expect("measured").1;
    let largest = written_at.last().expect("measured").1;
    assert!(
        largest <= smallest + 2,
        "node count per one-key change grew from {smallest} to {largest} across \
         a fiftyfold change in set size"
    );
}

#[test]
fn an_untouched_subtree_is_never_read() {
    // Stronger than counting writes: carrying a subtree forward must not even
    // load it, or a large store still pays to commit a small change.
    struct Counting<'a> {
        inner: &'a Nodes,
        reads: std::cell::Cell<usize>,
    }
    impl NodeSource for Counting<'_> {
        fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
            self.reads.set(self.reads.get() + 1);
            self.inner.node(hash)
        }
    }

    let (nodes, root) = built(50_000);
    let counting = Counting {
        inner: &nodes,
        reads: std::cell::Cell::new(0),
    };
    let mut sink = NodeSink::default();
    apply(&counting, root, vec![set(1, b"changed")], &mut sink).expect("apply");
    assert!(
        counting.reads.get() <= 8,
        "a one-key change read {} nodes from a 50,000-entry index",
        counting.reads.get()
    );
}

#[test]
fn a_batch_shares_the_paths_its_keys_share() {
    // Committing 100 changes must cost far less than 100 separate commits, or
    // batching is decoration.
    let (mut nodes, root) = built(20_000);
    let changes: Vec<IndexChange> = (0..100).map(|n| set(n, b"batched")).collect();
    let mut batch_sink = NodeSink::default();
    let batched = apply(&nodes, root, changes.clone(), &mut batch_sink).expect("batch");

    let mut one_at_a_time = root;
    let mut total = 0usize;
    for change in changes {
        let mut sink = NodeSink::default();
        one_at_a_time = apply(&nodes, one_at_a_time, vec![change], &mut sink).expect("single");
        total += sink.written.len();
        nodes.absorb(sink);
    }
    assert_eq!(batched, one_at_a_time, "batching changed the result");
    // 100 hashed keys land in ~100 distinct leaves at this size, so the leaves
    // are not what batching saves — the ancestors are. One at a time pays the
    // path above each leaf 100 times; a batch pays it once, which at this depth
    // is a two-thirds saving.
    let batched_nodes = batch_sink.written.len();
    assert!(
        batched_nodes * 2 < total,
        "a batch of 100 wrote {batched_nodes} nodes against {total} written one \
         at a time — the shared ancestry is supposed to be written once"
    );
    assert!(
        batched_nodes <= 110,
        "a batch of 100 keys wrote {batched_nodes} nodes; beyond ~100 leaves \
         plus a shared spine, something is rewriting paths it need not"
    );
}

#[test]
fn every_key_is_found_and_absent_keys_are_not() {
    let (nodes, root) = built(3_000);
    for n in [0u64, 1, 1_500, 2_999] {
        assert_eq!(
            lookup(&nodes, root, &key(n)).expect("lookup"),
            Some(n.to_be_bytes().to_vec())
        );
    }
    assert_eq!(lookup(&nodes, root, &key(3_000)).expect("lookup"), None);
    assert_eq!(lookup(&nodes, None, &key(0)).expect("lookup"), None);
}

#[test]
fn a_stream_yields_every_entry_in_key_order() {
    let (nodes, root) = built(2_000);
    let mut seen: Vec<IndexKey> = Vec::new();
    let count = stream(&nodes, root, &mut |e| seen.push(e.key)).expect("stream");
    assert_eq!(count, 2_000);
    assert_eq!(seen.len(), 2_000);
    assert!(
        seen.windows(2).all(|w| w[0] < w[1]),
        "stream must be ordered"
    );
}

#[test]
fn the_spine_is_small_relative_to_the_entry_set() {
    // What lets a sweep probe entries by lookup rather than materialising every
    // required hash.
    let (nodes, root) = built(20_000);
    let spine = spine(&nodes, root).expect("spine");
    assert!(
        (spine.len() as u64) * 20 < 20_000,
        "a 20,000-entry index has a {}-node spine — too large to hold",
        spine.len()
    );
}

#[test]
fn validation_accepts_what_the_builder_produces() {
    for count in [1u64, 2, 256, 257, 4_000] {
        let (nodes, root) = built(count);
        assert_eq!(validate(&nodes, root), Ok(count), "at {count} entries");
    }
}

#[test]
fn a_non_canonical_shape_is_refused() {
    // A tree can be internally consistent and still not be the encoding this
    // crate produces. Accepting one would mean a set had more than one root.
    use journal::index::IndexNode;

    // Force a leaf that fits into a branch: legal hashes, legal counts, wrong
    // shape.
    let mut nodes = Nodes::default();
    let mut sink = NodeSink::default();
    let leaf_a = IndexNode::Leaf(vec![entry(0, b"a")]);
    let leaf_b = IndexNode::Leaf(vec![entry(1, b"b")]);
    let a_bytes = leaf_a.encode();
    let b_bytes = leaf_b.encode();
    let mut children: [Option<ChildRef>; 16] = Default::default();
    children[journal_test_nibble(&key(0))] = Some(ChildRef {
        hash: node_hash(&a_bytes),
        count: 1,
    });
    children[journal_test_nibble(&key(1))] = Some(ChildRef {
        hash: node_hash(&b_bytes),
        count: 1,
    });
    let branch = IndexNode::Branch(Box::new(children));
    let branch_bytes = branch.encode();
    sink.written.push(a_bytes.clone());
    sink.written.push(b_bytes.clone());
    sink.written.push(branch_bytes.clone());
    nodes.absorb(sink);

    let root = Some(ChildRef {
        hash: node_hash(&branch_bytes),
        count: 2,
    });
    assert_eq!(
        validate(&nodes, root),
        Err(IndexError::NotCanonicalShape),
        "two entries belong in one leaf; a branch over them is not canonical"
    );
}

/// The first nibble of a key — mirrors the module's private helper so a test
/// can construct a deliberately wrong tree.
fn journal_test_nibble(key: &IndexKey) -> usize {
    (key[0] >> 4) as usize
}

#[test]
fn a_count_that_lies_is_refused() {
    use journal::index::IndexNode;
    let (mut nodes, root) = built(1_000);
    let root = root.expect("non-empty");
    let bytes = nodes.node(&root.hash).expect("root node");
    let IndexNode::Branch(children) = IndexNode::decode_canonical(&bytes).expect("decode") else {
        panic!("a 1,000-entry index roots at a branch");
    };
    // Re-emit the same branch under a count that does not match its subtree.
    let lying = IndexNode::Branch(children);
    let lying_bytes = lying.encode();
    let mut sink = NodeSink::default();
    sink.written.push(lying_bytes.clone());
    nodes.absorb(sink);
    assert_eq!(
        validate(
            &nodes,
            Some(ChildRef {
                hash: node_hash(&lying_bytes),
                count: 999,
            })
        ),
        Err(IndexError::CountMismatch)
    );
}

#[test]
fn a_missing_node_is_named_rather_than_papered_over() {
    let (_nodes, root) = built(1_000);
    let root = root.expect("non-empty");
    let empty = Nodes::default();
    assert_eq!(
        validate(&empty, Some(root)),
        Err(IndexError::MissingNode(root.hash))
    );
}

#[test]
fn bounds_are_enforced_before_anything_is_built() {
    let mut sink = NodeSink::default();
    let oversize = vec![IndexEntry {
        key: key(0),
        value: vec![0u8; MAX_VALUE_BYTES + 1],
    }];
    assert_eq!(build_index(oversize, &mut sink), Err(IndexError::Bounds));
    assert!(sink.written.is_empty(), "nothing may be written on refusal");

    let nodes = Nodes::default();
    let mut sink = NodeSink::default();
    assert_eq!(
        apply(
            &nodes,
            None,
            vec![IndexChange {
                key: key(0),
                value: Some(vec![0u8; MAX_VALUE_BYTES + 1]),
            }],
            &mut sink,
        ),
        Err(IndexError::Bounds)
    );
}

#[test]
fn duplicate_keys_are_refused_rather_than_silently_resolved() {
    let mut sink = NodeSink::default();
    assert_eq!(
        build_index(vec![entry(0, b"a"), entry(0, b"b")], &mut sink),
        Err(IndexError::Order)
    );
}

#[test]
fn a_large_value_still_indexes_and_reads_back() {
    // Body records are the largest values this index carries; the leaf byte
    // bound must split rather than refuse.
    let big = vec![0xABu8; MAX_VALUE_BYTES];
    let mut nodes = Nodes::default();
    let mut sink = NodeSink::default();
    let entries: Vec<IndexEntry> = (0..64).map(|n| entry(n, &big)).collect();
    let root = build_index(entries, &mut sink).expect("build");
    nodes.absorb(sink);
    assert_eq!(validate(&nodes, root), Ok(64));
    assert_eq!(lookup(&nodes, root, &key(7)).expect("lookup"), Some(big));
}

#[test]
fn a_thousand_random_operations_agree_with_a_plain_map() {
    // Differential test against the obvious implementation. Deterministic
    // sequence, so a failure is reproducible.
    let mut model: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut nodes = Nodes::default();
    let mut root = None;
    let mut state = 0x243F_6A88_85A3_08D3u64;

    for step in 0..1_000u64 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let n = (state >> 33) % 400;
        let change = if state % 3 == 0 {
            model.remove(&n);
            remove(n)
        } else {
            let value = step.to_be_bytes().to_vec();
            model.insert(n, value.clone());
            set(n, &value)
        };
        let mut sink = NodeSink::default();
        root = apply(&nodes, root, vec![change], &mut sink).expect("apply");
        nodes.absorb(sink);
    }

    assert_eq!(validate(&nodes, root), Ok(model.len() as u64));
    for (n, value) in &model {
        assert_eq!(
            lookup(&nodes, root, &key(*n)).expect("lookup").as_ref(),
            Some(value),
            "key {n} disagreed with the model"
        );
    }

    // And the incrementally-maintained root equals a rebuild of the same set.
    let mut sink = NodeSink::default();
    let rebuilt = build_index(
        model
            .iter()
            .map(|(n, v)| IndexEntry {
                key: key(*n),
                value: v.clone(),
            })
            .collect(),
        &mut sink,
    )
    .expect("rebuild");
    assert_eq!(
        root, rebuilt,
        "a thousand edits drifted from the canonical encoding of the set"
    );
}
