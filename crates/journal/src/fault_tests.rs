//! Fault-injection matrix for the pack-served store: a crash at every named
//! commit boundary must recover to the complete old state — the flush is the
//! only switch, and nothing after it can fail a commit. Plus integrity
//! classification, detached collection, deferred laziness, lease semantics,
//! and required-set behavior — the same contracts the file-per-object format
//! carried, restated against the pack.
//!
//! Two old tests have no descendant here, on purpose. Sequence reuse after a
//! deleted counter is structurally impossible now — the seal *is* the
//! counter. And the oversized-sparse-payload attack has no surface — the
//! pack table, not file metadata, is the length authority (the hostile-length
//! read below keeps the observable claim).

use journal::{Deferred, Failure, Index, Object, Store, FAULT_POINTS};

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-journal-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a *prior-layout* store whose deferred index carries `count` entries
/// — by hand, exactly as the old format put it on disk. Opening it now also
/// exercises the migration door, which is the point: these fixtures are the
/// stores real machines still hold.
fn install_deferred_index_fixture(
    root: &std::path::Path,
    count: u32,
) -> (journal::index::ChildRef, Vec<journal::index::IndexEntry>) {
    std::fs::create_dir_all(root.join("objects")).unwrap();
    std::fs::create_dir_all(root.join("journal")).unwrap();
    let value = {
        let mut value = vec![2u8];
        value.extend_from_slice(&1u64.to_be_bytes());
        value
    };
    let entries: Vec<journal::index::IndexEntry> = (0..count)
        .map(|ordinal| journal::index::IndexEntry {
            key: journal::object_content_hash(&ordinal.to_be_bytes()),
            value: value.clone(),
        })
        .collect();
    let mut sink = journal::index::NodeSink::default();
    let root_ref = journal::index::build_index(entries.clone(), &mut sink)
        .unwrap()
        .unwrap();
    for bytes in sink.written {
        let hash = journal::object_content_hash(&bytes);
        let name = hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        std::fs::write(root.join("objects").join(name), bytes).unwrap();
    }
    let manifest = journal::Manifest {
        format_version: journal::STORE_FORMAT_VERSION,
        sequence: 1,
        eager_object_index_root: None,
        deferred_object_index_root: Some((root_ref.hash, root_ref.count)),
        caller_meta: None,
        caller_index_roots: Vec::new(),
        lazy_caller_index_roots: Vec::new(),
    };
    std::fs::write(
        root.join("current-manifest"),
        postcard::to_stdvec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("counter"), 1u64.to_le_bytes()).unwrap();
    (root_ref, entries)
}

/// Walk a live store's deferred index down to the leaf holding `key`,
/// through the store's own reads.
fn leaf_for_key(store: &Store, mut current: journal::index::ChildRef, key: &[u8; 32]) -> [u8; 32] {
    for depth in 0..journal::index::MAX_DEPTH {
        let bytes = store.read(&current.hash).unwrap();
        match journal::index::IndexNode::decode_canonical(&bytes).unwrap() {
            journal::index::IndexNode::Leaf(_) => return current.hash,
            journal::index::IndexNode::Branch(children) => {
                let byte = key[depth / 2];
                let slot = if depth.is_multiple_of(2) {
                    usize::from(byte >> 4)
                } else {
                    usize::from(byte & 0x0f)
                };
                current = children[slot].unwrap();
            }
        }
    }
    panic!("path did not reach a leaf")
}

#[test]
fn commits_roundtrip_and_sequences_are_monotone() {
    let root = temp_root("happy");
    let mut store = Store::open(&root).unwrap();
    assert!(store.manifest().is_none(), "fresh store has no manifest");

    let s1 = store
        .commit(
            &[b"object-one".to_vec()],
            &[],
            Index::NONE,
            b"meta-1".to_vec(),
        )
        .unwrap();
    let s2 = store
        .commit(
            &[b"object-two".to_vec()],
            &[],
            Index::NONE,
            b"meta-2".to_vec(),
        )
        .unwrap();
    assert!(s2 > s1);

    // Reopen: the second manifest is current and both objects verify. Objects
    // accumulate — a commit names what it adds and what it drops, so a caller
    // that forgets to mention an object keeps it.
    drop(store);
    let store = Store::open(&root).unwrap();
    let manifest = store.manifest().unwrap().clone();
    assert_eq!(manifest.sequence, s2);
    assert_eq!(store.caller_meta().unwrap().unwrap(), b"meta-2");
    let required = store.required_objects().unwrap();
    assert_eq!(required.len(), 2);
    let contents: Vec<Vec<u8>> = required
        .iter()
        .map(|o| store.read_object(o).unwrap())
        .collect();
    assert!(contents.contains(&b"object-one".to_vec()));
    assert!(contents.contains(&b"object-two".to_vec()));
}

#[test]
fn dropping_a_requirement_collects_the_object_but_not_before() {
    // The other half of accumulate-by-default: a caller says when something
    // stops being required, and the bytes survive until a sweep, so a reader
    // holding a reference across the commit cannot tear.
    let root = temp_root("drop");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(&[b"first".to_vec()], &[], Index::NONE, b"m1".to_vec())
        .unwrap();
    let first = store.required_objects().unwrap()[0];
    store
        .commit(
            &[b"second".to_vec()],
            &[first.hash],
            Index::NONE,
            b"m2".to_vec(),
        )
        .unwrap();

    assert!(!store.is_required(&first.hash).unwrap());
    assert_eq!(
        store.read_object(&first).unwrap(),
        b"first",
        "the bytes outlive the requirement until a sweep"
    );

    drop(store);
    let store = Store::open(&root).unwrap();
    let required = store.required_objects().unwrap();
    assert_eq!(required.len(), 1);
    assert_eq!(store.read_object(&required[0]).unwrap(), b"second");
    assert_eq!(store.read_object(&first).unwrap(), b"first");
    store.collect_unreachable().unwrap();
    assert!(
        store.read_object(&first).is_err(),
        "swept by detached maintenance"
    );
}

#[test]
fn a_full_required_set_commit_computes_its_own_removals() {
    let root = temp_root("fullset");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(
            &[b"a".to_vec(), b"b".to_vec()],
            &[],
            Index::NONE,
            b"m1".to_vec(),
        )
        .unwrap();
    let keep: Vec<Object> = store
        .required_objects()
        .unwrap()
        .into_iter()
        .filter(|o| store.read_object(o).unwrap() == b"a")
        .collect();
    store
        .commit_required_set(&[b"c".to_vec()], &keep, b"m2".to_vec())
        .unwrap();

    let mut contents: Vec<Vec<u8>> = store
        .required_objects()
        .unwrap()
        .iter()
        .map(|o| store.read_object(o).unwrap())
        .collect();
    contents.sort();
    assert_eq!(contents, vec![b"a".to_vec(), b"c".to_vec()]);
}

#[test]
fn a_crash_at_every_fault_point_recovers_the_old_state() {
    // Every named point precedes the flush, so every injected crash fails
    // the call, leaves the old state, and stays retryable. There are no
    // post-authoritative points any more: after the flush, nothing can fail
    // a commit, which is the discipline the old format spent two extra
    // journal phases to approximate.
    for &point in &FAULT_POINTS {
        let root = temp_root(&format!("fault-{point}"));

        let mut store = Store::open(&root).unwrap();
        let s1 = store
            .commit(
                &[b"old-object".to_vec()],
                &[],
                Index::NONE,
                b"old-meta".to_vec(),
            )
            .unwrap();
        drop(store);

        let mut faulty = Store::open(&root)
            .unwrap()
            .with_fault_injector(Box::new(move |name| name == point));
        let lazy = [b"new-object".to_vec()];
        let result = faulty.commit_classified(
            &[],
            &[],
            Deferred {
                added: &lazy,
                removed: &[],
            },
            Index::NONE,
            b"new-meta".to_vec(),
        );
        assert!(
            matches!(result.unwrap_err(), Failure::Operation { .. }),
            "{point}: a pre-flush crash surfaces as a retryable operation failure"
        );
        drop(faulty);

        let store = Store::open(&root).unwrap_or_else(|e| panic!("{point}: recovery failed: {e}"));
        assert_eq!(
            store.caller_meta().unwrap().unwrap(),
            b"old-meta",
            "{point}: recovered to the wrong state"
        );
        let required = store.required_objects().unwrap();
        assert!(
            required
                .iter()
                .any(|o| store.read_object(o).unwrap() == b"old-object"),
            "{point}: recovered object content"
        );

        let mut store = store;
        let s3 = store
            .commit(
                &[b"after".to_vec()],
                &[],
                Index::NONE,
                b"after-meta".to_vec(),
            )
            .unwrap();
        assert!(s3 > s1, "{point}: sequence must move strictly forward");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn a_bogus_carried_reference_fails_the_commit_up_front() {
    let root = temp_root("bogus-keep");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(&[b"real".to_vec()], &[], Index::NONE, b"m1".to_vec())
        .unwrap();
    let bogus = Object {
        hash: [0xEE; 32],
        len: 4,
    };
    let err = store
        .commit_required_set(&[b"newer".to_vec()], &[bogus], b"m2".to_vec())
        .unwrap_err();
    assert!(matches!(err, Failure::Integrity(_)));
    drop(store);
    let store = Store::open(&root).unwrap();
    assert_eq!(store.caller_meta().unwrap().unwrap(), b"m1");
}

#[test]
fn a_corrupt_object_is_an_integrity_failure_not_a_repair() {
    let root = temp_root("corrupt");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(&[b"precious".to_vec()], &[], Index::NONE, b"m1".to_vec())
        .unwrap();
    let precious = journal::object_content_hash(b"precious");
    // A second commit, so the rot lands in settled history. Rot in the very
    // last seal's delta is physically indistinguishable from a torn commit
    // and recovery steps past it — the WAL trade-off — but rot anywhere the
    // eager verification walks is a fail-stop, never a repair.
    store
        .commit(&[b"later".to_vec()], &[], Index::NONE, b"m2".to_vec())
        .unwrap();
    drop(store);

    assert!(journal::corrupt_object_for_test(&root, &precious));
    match Store::open(&root) {
        Err(Failure::Integrity(_)) => {}
        other => panic!("expected Integrity, got {other:?}"),
    }
}

#[test]
fn detached_collection_removes_the_unrequired_and_keeps_the_reachable() {
    let root = temp_root("gc");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(&[b"kept".to_vec()], &[], Index::NONE, b"m".to_vec())
        .unwrap();
    // Litter, the only way it can exist now: bytes whose requirement was
    // dropped. (Stray files cannot be planted inside a pack.)
    let litter = journal::object_content_hash(b"litter");
    store
        .commit(&[b"litter".to_vec()], &[], Index::NONE, b"m2".to_vec())
        .unwrap();
    store
        .commit(&[], &[litter], Index::NONE, b"m3".to_vec())
        .unwrap();
    drop(store);

    let store = Store::open(&root).unwrap();
    store.collect_unreachable().unwrap();
    assert!(
        store.read(&litter).is_err(),
        "an unreferenced object is collected"
    );
    assert_eq!(
        store
            .read_object(&store.required_objects().unwrap()[0])
            .unwrap(),
        b"kept"
    );
    assert_eq!(store.caller_meta().unwrap().unwrap(), b"m3");
}

#[test]
fn deferred_payloads_are_not_read_at_open_and_fail_typed_on_exact_read() {
    let root = temp_root("lazy-corrupt");
    let payload = vec![0x5au8; 2 * 1024 * 1024];
    let reference = Object {
        hash: journal::object_content_hash(&payload),
        len: u64::try_from(payload.len()).unwrap(),
    };
    let mut store = Store::open(&root).unwrap();
    store
        .commit_classified(
            &[],
            &[],
            Deferred {
                added: &[payload],
                removed: &[],
            },
            Index::NONE,
            b"lazy".to_vec(),
        )
        .unwrap();
    // A later commit, so the rotted payload is not the newest seal's delta.
    store
        .commit(&[b"bump".to_vec()], &[], Index::NONE, b"bump".to_vec())
        .unwrap();
    drop(store);
    assert!(journal::corrupt_object_for_test(&root, &reference.hash));

    // If open verified even one deferred payload, it would fail here.
    journal::watch_recovery_object_reads(reference.hash);
    let store = Store::open(&root).expect("deferred payload is verified lazily");
    assert_eq!(
        journal::watched_recovery_object_reads(),
        0,
        "recovery performed no read of the watched deferred payload"
    );
    assert!(store.is_required(&reference.hash).unwrap());
    assert!(matches!(
        store.reader().read_object(&reference),
        Err(Failure::Integrity(
            journal::Defect::MissingObject | journal::Defect::CorruptObject
        ))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deferred_open_reads_one_root_at_one_hundred_thousand_entries() {
    let root = temp_root("lazy-index-100k");
    install_deferred_index_fixture(&root, 100_000);
    // The first open migrates the prior layout wholesale; laziness is the
    // steady state's claim, so measure the reopen.
    drop(Store::open(&root).expect("the prior layout migrates"));
    let store = Store::open(&root).expect("open authenticates only the deferred root");
    assert_eq!(
        journal::recovery_index_node_reads(),
        1,
        "deferred recovery cost is root-sized, not entry-sized"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn lazy_caller_open_reads_one_root_at_one_hundred_thousand_entries() {
    let root = temp_root("lazy-caller-100k");
    let (index, _) = install_deferred_index_fixture(&root, 100_000);
    // Re-point the fixture's manifest before anything migrates it.
    let path = root.join("current-manifest");
    let bytes = std::fs::read(&path).unwrap();
    let mut manifest: journal::Manifest = postcard::from_bytes(&bytes).unwrap();
    manifest.deferred_object_index_root = None;
    manifest.lazy_caller_index_roots = vec![(index.hash, index.count)];
    std::fs::write(path, postcard::to_stdvec(&manifest).unwrap()).unwrap();

    drop(Store::open(&root).expect("the prior layout migrates"));
    let store = Store::open(&root).expect("open authenticates only the ownership root");
    assert_eq!(journal::recovery_index_node_reads(), 1);
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn corrupt_unopened_deferred_leaf_fails_lookup_and_scrub_without_collecting() {
    let root = temp_root("lazy-corrupt-leaf");
    let mut store = Store::open(&root).unwrap();
    let payloads: Vec<Vec<u8>> = (0..4_096u32).map(|n| n.to_be_bytes().to_vec()).collect();
    let target = journal::object_content_hash(&payloads[0]);
    store
        .commit_classified(
            &[],
            &[],
            Deferred {
                added: &payloads,
                removed: &[],
            },
            Index::NONE,
            b"deep".to_vec(),
        )
        .unwrap();
    // An orphan that corrupt-index collection must never touch.
    let orphan = journal::object_content_hash(b"must-not-be-collected");
    store
        .commit(
            &[b"must-not-be-collected".to_vec()],
            &[],
            Index::NONE,
            b"o1".to_vec(),
        )
        .unwrap();
    store
        .commit(&[], &[orphan], Index::NONE, b"o2".to_vec())
        .unwrap();
    let deferred_root = journal::index::ChildRef {
        hash: store
            .manifest()
            .unwrap()
            .deferred_object_index_root
            .unwrap()
            .0,
        count: store
            .manifest()
            .unwrap()
            .deferred_object_index_root
            .unwrap()
            .1,
    };
    let leaf = leaf_for_key(&store, deferred_root, &target);
    drop(store);
    assert!(journal::corrupt_object_for_test(&root, &leaf));

    let store = Store::open(&root).expect("unopened deferred subtrees are lazy");
    assert!(matches!(
        store.is_required(&target),
        Err(Failure::Integrity(journal::Defect::CorruptIndex))
    ));
    assert!(matches!(
        store.collect_unreachable(),
        Err(Failure::Integrity(journal::Defect::CorruptIndex))
    ));
    assert_eq!(
        store.read(&orphan).unwrap(),
        b"must-not-be-collected",
        "scrub failure never guesses reachability"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deferred_reader_enforces_hostile_length_and_gc_lease() {
    let root = temp_root("lazy-pin");
    let payload = b"leased-payload".to_vec();
    let reference = Object {
        hash: journal::object_content_hash(&payload),
        len: u64::try_from(payload.len()).unwrap(),
    };
    let mut store = Store::open(&root).unwrap();
    store
        .commit_classified(
            &[],
            &[],
            Deferred {
                added: &[payload],
                removed: &[],
            },
            Index::NONE,
            b"pinned".to_vec(),
        )
        .unwrap();
    let reader = store.reader();
    let hostile = Object {
        len: reference.len.saturating_add(1),
        ..reference
    };
    assert!(matches!(
        reader.read_object(&hostile),
        Err(Failure::Integrity(journal::Defect::CorruptObject))
    ));
    drop(reader);

    store
        .commit_classified(
            &[],
            &[],
            Deferred {
                added: &[],
                removed: &[reference.hash],
            },
            Index::NONE,
            b"unpinned-from-manifest".to_vec(),
        )
        .unwrap();
    let reader = store.reader();
    let lease = reader.pin_objects(&[reference]);
    store.collect_unreachable().unwrap();
    // The lease is what carried the bytes into the new generation: even a
    // reader taken AFTER the sweep still finds them.
    assert_eq!(
        store.reader().read_object(&reference).unwrap(),
        b"leased-payload",
        "a pinned exact publication survives detached GC"
    );
    drop(lease);
    drop(reader);
    store.collect_unreachable().unwrap();
    assert!(matches!(
        store.reader().read_object(&reference),
        Err(Failure::Integrity(journal::Defect::MissingObject))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deferred_root_lease_keeps_a_whole_old_publication_without_enumeration() {
    let root = temp_root("lazy-root-pin");
    let payload = b"root-leased-payload".to_vec();
    let reference = Object {
        hash: journal::object_content_hash(&payload),
        len: u64::try_from(payload.len()).unwrap(),
    };
    let mut store = Store::open(&root).unwrap();
    store
        .commit_classified(
            &[],
            &[],
            Deferred {
                added: &[payload],
                removed: &[],
            },
            Index::NONE,
            b"root-pinned".to_vec(),
        )
        .unwrap();
    // Reader creation pins one root coordinate, not every entry it reaches.
    let old_publication = store.reader();
    store
        .commit_classified(
            &[],
            &[],
            Deferred {
                added: &[],
                removed: &[reference.hash],
            },
            Index::NONE,
            b"current-no-longer-needs-it".to_vec(),
        )
        .unwrap();
    store.collect_unreachable().unwrap();
    // The root lease kept the whole publication reachable through the sweep:
    // the old reader still resolves it, and so does a fresh one, because the
    // leased tree's objects were carried into the new generation.
    assert_eq!(
        old_publication.read_object(&reference).unwrap(),
        b"root-leased-payload"
    );
    assert_eq!(store.read(&reference.hash).unwrap(), b"root-leased-payload");
    drop(old_publication);
    store.collect_unreachable().unwrap();
    assert!(matches!(
        store.reader().read_object(&reference),
        Err(Failure::Integrity(journal::Defect::MissingObject))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn required_set_tracks_live_state_not_commit_history() {
    // The store's promise, and the one that makes open affordable: a commit
    // costs what it changed, and what the store keeps is what it holds.
    let root = temp_root("required-tracks-live");
    let mut store = Store::open(&root).unwrap();

    /// `index::apply` needs sealed nodes; the store's own reads serve them.
    struct StoreNodes<'a>(&'a Store);
    impl journal::index::NodeSource for StoreNodes<'_> {
        fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
            self.0.read(hash).ok()
        }
    }

    let mut caller_root: Option<journal::index::ChildRef> = None;
    let mut previous: Option<[u8; 32]> = None;
    for round in 0..64u32 {
        let object = format!("value-{round}").into_bytes();
        let hash = journal::object_content_hash(&object);
        let mut sink = journal::index::NodeSink::default();
        caller_root = journal::index::apply(
            &StoreNodes(&store),
            caller_root,
            vec![journal::index::IndexChange {
                key: [7u8; 32],
                value: Some(hash.to_vec()),
            }],
            &mut sink,
        )
        .unwrap();
        let removed: Vec<[u8; 32]> = previous.into_iter().collect();
        let roots: Vec<([u8; 32], u64)> = caller_root
            .into_iter()
            .map(|root| (root.hash, root.count))
            .collect();
        store
            .commit(
                &[object],
                &removed,
                Index {
                    roots: &roots,
                    lazy_roots: &[],
                    nodes: &sink.written,
                },
                format!("meta-{round}").into_bytes(),
            )
            .unwrap();
        previous = Some(hash);
        store.collect_unreachable().unwrap();
    }

    let required = store.required_objects().unwrap();
    assert_eq!(
        required.len(),
        1,
        "one live object after 64 rewrites, not 64: {required:?}"
    );

    // And a reopen agrees, rather than reclaiming what the session should
    // have.
    drop(store);
    let reopened = Store::open(&root).unwrap();
    assert_eq!(reopened.required_objects().unwrap().len(), 1);
}
