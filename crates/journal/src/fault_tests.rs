//! Fault-injection matrix for the journaled store: a crash at every named
//! write/fsync/rename/journal boundary must recover to the complete old state —
//! or the complete new one when only the acknowledgment was lost — never a
//! mixture. Plus integrity classification, orphan GC, counter monotonicity,
//! and required-set semantics.

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

fn leaf_for_key(
    root: &std::path::Path,
    mut current: journal::index::ChildRef,
    key: &[u8; 32],
) -> [u8; 32] {
    for depth in 0..journal::index::MAX_DEPTH {
        let name = current
            .hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let bytes = std::fs::read(root.join("objects").join(name)).unwrap();
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
    panic!("fixture path did not reach a leaf")
}

fn make_fixture_a_lazy_caller_index(root: &std::path::Path, index: journal::index::ChildRef) {
    let path = root.join("current-manifest");
    let bytes = std::fs::read(&path).unwrap();
    let mut manifest: journal::Manifest = postcard::from_bytes(&bytes).unwrap();
    manifest.deferred_object_index_root = None;
    manifest.lazy_caller_index_roots = vec![(index.hash, index.count)];
    std::fs::write(path, postcard::to_stdvec(&manifest).unwrap()).unwrap();
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
    // that forgets to mention an object keeps it. The shape this replaced had
    // callers re-declare everything they wanted kept, where forgetting deleted.
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

    // Open is payload-directory independent. Detached maintenance performs the
    // mark/sweep explicitly, and only then is the orphan gone.
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
    // `commit_required_set` is the O(total) door for callers whose set is small
    // enough that enumerating it is not the cost. It must agree with the delta
    // door about what the store ends up holding.
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
fn a_crash_at_every_fault_point_recovers_to_a_complete_state() {
    for &point in &FAULT_POINTS {
        let root = temp_root(&format!("fault-{point}"));

        // Baseline: one committed state.
        let mut store = Store::open(&root).unwrap();
        let s1 = store
            .commit(
                &[b"old-object".to_vec()],
                &[],
                Index::NONE,
                b"old-meta".to_vec(),
            )
            .unwrap();

        // Attempt a second commit that "crashes" at the named point. The
        // acknowledgment discipline: every point before the manifest rename
        // fails the call and leaves the old state; the two post-authoritative
        // cleanup points lose only cleanup — the call MUST still succeed,
        // because a durably committed operation may never be reported as a
        // retryable failure (a retry would apply it twice).
        let expect_new = matches!(point, "journal-committed" | "journal-remove");
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
        if expect_new {
            result.unwrap_or_else(|e| {
                panic!("{point}: post-authoritative cleanup crash must not fail the commit: {e}")
            });
        } else {
            assert!(
                matches!(result.unwrap_err(), Failure::Operation { .. }),
                "{point}: pre-authoritative crash surfaces as Durability"
            );
        }
        drop(faulty);

        // Recovery must expose ONE complete state matching the acknowledgment.
        let store = Store::open(&root).unwrap_or_else(|e| panic!("{point}: recovery failed: {e}"));
        store
            .manifest()
            .unwrap_or_else(|| panic!("{point}: a committed store never loses its manifest"));
        let (want_meta, want_obj): (&[u8], &[u8]) = if expect_new {
            (b"new-meta", b"new-object")
        } else {
            (b"old-meta", b"old-object")
        };
        assert_eq!(
            store.caller_meta().unwrap().unwrap(),
            want_meta,
            "{point}: recovered to the wrong state"
        );
        let required = store.required_objects().unwrap();
        assert!(
            required
                .iter()
                .any(|o| store.read_object(o).unwrap() == want_obj),
            "{point}: recovered object content"
        );

        // The store keeps working, and sequences never reuse: every commit
        // after recovery is strictly beyond the baseline (gaps allowed).
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
    // A keep ref naming an object that does not exist must refuse the commit
    // BEFORE anything lands — otherwise a "successful" commit would fail
    // integrity on the next open.
    let bogus = Object {
        hash: [0xEE; 32],
        len: 4,
    };
    let err = store
        .commit_required_set(&[b"newer".to_vec()], &[bogus], b"m2".to_vec())
        .unwrap_err();
    assert!(matches!(err, Failure::Integrity(_)));
    // The store is untouched and still healthy.
    drop(store);
    let store = Store::open(&root).unwrap();
    assert_eq!(store.caller_meta().unwrap().unwrap(), b"m1");
}

#[test]
fn a_corrupt_object_is_an_integrity_failure_not_a_repair() {
    let root = temp_root("corrupt");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(&[b"precious".to_vec()], &[], Index::NONE, b"m".to_vec())
        .unwrap();
    drop(store);

    // Corrupt the object on disk.
    let objects_dir = root.join("objects");
    let entry = std::fs::read_dir(&objects_dir)
        .unwrap()
        .flatten()
        .next()
        .unwrap();
    std::fs::write(entry.path(), b"tampered").unwrap();

    match Store::open(&root) {
        Err(Failure::Integrity(_)) => {}
        other => panic!("expected Integrity, got {other:?}"),
    }
}

#[test]
fn a_missing_counter_on_a_committed_store_fails_closed() {
    let root = temp_root("counter");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(&[b"x".to_vec()], &[], Index::NONE, b"m".to_vec())
        .unwrap();
    drop(store);

    std::fs::remove_file(root.join("counter")).unwrap();
    match Store::open(&root) {
        Err(Failure::Integrity(_)) => {}
        other => panic!("expected Integrity (no sequence reuse), got {other:?}"),
    }
}

#[test]
fn detached_collection_removes_orphans_and_temps() {
    let root = temp_root("gc");
    let mut store = Store::open(&root).unwrap();
    store
        .commit(&[b"kept".to_vec()], &[], Index::NONE, b"m".to_vec())
        .unwrap();
    drop(store);

    // Litter: a stray temp and an unreferenced (fake) object.
    std::fs::write(root.join("objects").join("deadbeef.tmp"), b"junk").unwrap();
    std::fs::write(root.join("objects").join("ab".repeat(32)), b"junk").unwrap();

    let store = Store::open(&root).unwrap();
    store.collect_unreachable().unwrap();
    let names: Vec<String> = std::fs::read_dir(root.join("objects"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|n| n.ends_with(".tmp")),
        "temps are collected: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == &"ab".repeat(32)),
        "an unreferenced object is collected: {names:?}"
    );
    // What survives is the required object, the index nodes that reach it, and
    // the caller's metadata object — all reachable, none of them litter.
    assert_eq!(
        store
            .read_object(&store.required_objects().unwrap()[0])
            .unwrap(),
        b"kept"
    );
    assert_eq!(store.caller_meta().unwrap().unwrap(), b"m");
}

#[test]
fn deferred_payloads_are_not_read_at_open_and_fail_typed_on_exact_read() {
    for (tag, remove) in [("corrupt", false), ("missing", true)] {
        let root = temp_root(&format!("lazy-{tag}"));
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
        drop(store);
        let path = root.join("objects").join(
            reference
                .hash
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        );
        if remove {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::write(&path, b"hostile").unwrap();
        }

        // If open touched even one deferred payload, this would fail here.
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
}

#[test]
fn deferred_open_reads_one_root_at_one_hundred_thousand_entries() {
    let root = temp_root("lazy-index-100k");
    install_deferred_index_fixture(&root, 100_000);
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
    make_fixture_a_lazy_caller_index(&root, index);
    let store = Store::open(&root).expect("open authenticates only the ownership root");
    assert_eq!(journal::recovery_index_node_reads(), 1);
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "release-scale one-million-entry deferred-index startup gate"]
fn deferred_open_reads_one_root_at_one_million_entries() {
    let root = temp_root("lazy-index-1m");
    let (index, _) = install_deferred_index_fixture(&root, 1_000_000);
    make_fixture_a_lazy_caller_index(&root, index);
    let store = Store::open(&root).expect("open authenticates only the deferred root");
    assert_eq!(journal::recovery_index_node_reads(), 1);
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn corrupt_unopened_deferred_leaf_fails_lookup_and_scrub_without_collecting() {
    let root = temp_root("lazy-corrupt-leaf");
    let (root_ref, entries) = install_deferred_index_fixture(&root, 4_096);
    let target = entries.first().unwrap().key;
    let leaf = leaf_for_key(&root, root_ref, &target);
    let leaf_path = root.join("objects").join(
        leaf.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    );
    std::fs::write(&leaf_path, b"corrupt unopened leaf").unwrap();
    let orphan = b"must-not-be-collected-on-corrupt-index";
    let orphan_hash = journal::object_content_hash(orphan);
    let orphan_path = root.join("objects").join(
        orphan_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    );
    std::fs::write(&orphan_path, orphan).unwrap();

    let store = Store::open(&root).expect("unopened deferred subtrees are lazy");
    assert_eq!(journal::recovery_index_node_reads(), 1);
    assert!(matches!(
        store.is_required(&target),
        Err(Failure::Integrity(journal::Defect::CorruptIndex))
    ));
    assert!(matches!(
        store.collect_unreachable(),
        Err(Failure::Integrity(journal::Defect::CorruptIndex))
    ));
    assert!(
        orphan_path.exists(),
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
    assert_eq!(
        reader.read_object(&reference).unwrap(),
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
    assert_eq!(
        old_publication.read_object(&reference).unwrap(),
        b"root-leased-payload"
    );
    drop(old_publication);
    store.collect_unreachable().unwrap();
    assert!(matches!(
        store.reader().read_object(&reference),
        Err(Failure::Integrity(journal::Defect::MissingObject))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn oversized_sparse_payload_is_rejected_before_bounded_allocation() {
    let root = temp_root("lazy-hostile-sparse");
    let payload = b"authenticated-small-payload".to_vec();
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
            b"sparse".to_vec(),
        )
        .unwrap();
    let path = root.join("objects").join(
        reference
            .hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    );
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(8 * 1024 * 1024 * 1024)
        .unwrap();
    assert!(matches!(
        store
            .reader()
            .read_object_bounded(&reference, reference.len),
        Err(Failure::Integrity(journal::Defect::CorruptObject))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn required_set_tracks_live_state_not_commit_history() {
    // The store's promise, and the one that makes open affordable: a commit
    // costs what it changed, and what the store keeps is what it holds. Index
    // nodes are kept alive by reachability from a root, so a rewrite orphans
    // the spine it superseded — admitting those nodes as *required* instead
    // would make every one of them permanent and the required set would grow
    // with the number of commits ever performed.
    let root = temp_root("required-tracks-live");
    let mut store = Store::open(&root).unwrap();

    // One caller index, rewritten in place 64 times. Live state is constant:
    // one entry, one object.
    let mut caller_root: Option<journal::index::ChildRef> = None;
    let mut previous: Option<[u8; 32]> = None;
    for round in 0..64u32 {
        let object = format!("value-{round}").into_bytes();
        let hash = journal::object_content_hash(&object);
        let mut sink = journal::index::NodeSink::default();
        caller_root = journal::index::apply(
            &journal::ObjectNodes { root: &store.root },
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

    let objects = std::fs::read_dir(root.join("objects")).unwrap().count();
    assert!(
        objects <= 4,
        "the object directory tracks live state: {objects} files after 64 commits"
    );

    // And a reopen agrees, rather than reclaiming what the session should have.
    drop(store);
    let reopened = Store::open(&root).unwrap();
    assert_eq!(reopened.required_objects().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_dir(root.join("objects")).unwrap().count(),
        objects,
        "an in-session sweep leaves nothing for the restart to find"
    );
}
