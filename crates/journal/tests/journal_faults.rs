//! Fault-injection matrix for the journaled store: a crash at every named
//! write/fsync/rename/journal boundary must recover to the complete old state —
//! or the complete new one when only the acknowledgment was lost — never a
//! mixture. Plus integrity classification, orphan GC, counter monotonicity,
//! and required-set semantics.

use journal::{CallerIndex, Failure, JournaledStore, ObjectRef, FAULT_POINTS};

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

#[test]
fn commits_roundtrip_and_sequences_are_monotone() {
    let root = temp_root("happy");
    let mut store = JournaledStore::open(&root).unwrap();
    assert!(store.manifest().is_none(), "fresh store has no manifest");

    let s1 = store
        .commit(
            &[b"object-one".to_vec()],
            &[],
            CallerIndex::NONE,
            b"meta-1".to_vec(),
        )
        .unwrap();
    let s2 = store
        .commit(
            &[b"object-two".to_vec()],
            &[],
            CallerIndex::NONE,
            b"meta-2".to_vec(),
        )
        .unwrap();
    assert!(s2 > s1);

    // Reopen: the second manifest is current and both objects verify. Objects
    // accumulate — a commit names what it adds and what it drops, so a caller
    // that forgets to mention an object keeps it. The shape this replaced had
    // callers re-declare everything they wanted kept, where forgetting deleted.
    let store = JournaledStore::open(&root).unwrap();
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
    let mut store = JournaledStore::open(&root).unwrap();
    store
        .commit(&[b"first".to_vec()], &[], CallerIndex::NONE, b"m1".to_vec())
        .unwrap();
    let first = store.required_objects().unwrap()[0];
    store
        .commit(
            &[b"second".to_vec()],
            &[first.hash],
            CallerIndex::NONE,
            b"m2".to_vec(),
        )
        .unwrap();

    assert!(!store.is_required(&first.hash).unwrap());
    assert_eq!(
        store.read_object(&first).unwrap(),
        b"first",
        "the bytes outlive the requirement until a sweep"
    );

    // The sweep runs at open, and only then is it gone.
    drop(store);
    let store = JournaledStore::open(&root).unwrap();
    let required = store.required_objects().unwrap();
    assert_eq!(required.len(), 1);
    assert_eq!(store.read_object(&required[0]).unwrap(), b"second");
    assert!(store.read_object(&first).is_err(), "swept at open");
}

#[test]
fn a_full_required_set_commit_computes_its_own_removals() {
    // `commit_required_set` is the O(total) door for callers whose set is small
    // enough that enumerating it is not the cost. It must agree with the delta
    // door about what the store ends up holding.
    let root = temp_root("fullset");
    let mut store = JournaledStore::open(&root).unwrap();
    store
        .commit(
            &[b"a".to_vec(), b"b".to_vec()],
            &[],
            CallerIndex::NONE,
            b"m1".to_vec(),
        )
        .unwrap();
    let keep: Vec<ObjectRef> = store
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
    for &point in FAULT_POINTS.iter() {
        let root = temp_root(&format!("fault-{point}"));

        // Baseline: one committed state.
        let mut store = JournaledStore::open(&root).unwrap();
        let s1 = store
            .commit(
                &[b"old-object".to_vec()],
                &[],
                CallerIndex::NONE,
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
        let mut faulty = JournaledStore::open(&root)
            .unwrap()
            .with_fault_injector(Box::new(move |name| name == point));
        let result = faulty.commit(
            &[b"new-object".to_vec()],
            &[],
            CallerIndex::NONE,
            b"new-meta".to_vec(),
        );
        if expect_new {
            result.unwrap_or_else(|e| {
                panic!("{point}: post-authoritative cleanup crash must not fail the commit: {e}")
            });
        } else {
            assert!(
                matches!(result.unwrap_err(), Failure::Durability(_)),
                "{point}: pre-authoritative crash surfaces as Durability"
            );
        }
        drop(faulty);

        // Recovery must expose ONE complete state matching the acknowledgment.
        let store =
            JournaledStore::open(&root).unwrap_or_else(|e| panic!("{point}: recovery failed: {e}"));
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
                CallerIndex::NONE,
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
    let mut store = JournaledStore::open(&root).unwrap();
    store
        .commit(&[b"real".to_vec()], &[], CallerIndex::NONE, b"m1".to_vec())
        .unwrap();
    // A keep ref naming an object that does not exist must refuse the commit
    // BEFORE anything lands — otherwise a "successful" commit would fail
    // integrity on the next open.
    let bogus = ObjectRef {
        hash: [0xEE; 32],
        len: 4,
    };
    let err = store
        .commit_required_set(&[b"newer".to_vec()], &[bogus], b"m2".to_vec())
        .unwrap_err();
    assert!(matches!(err, Failure::Integrity(_)));
    // The store is untouched and still healthy.
    drop(store);
    let store = JournaledStore::open(&root).unwrap();
    assert_eq!(store.caller_meta().unwrap().unwrap(), b"m1");
}

#[test]
fn a_corrupt_object_is_an_integrity_failure_not_a_repair() {
    let root = temp_root("corrupt");
    let mut store = JournaledStore::open(&root).unwrap();
    store
        .commit(
            &[b"precious".to_vec()],
            &[],
            CallerIndex::NONE,
            b"m".to_vec(),
        )
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

    match JournaledStore::open(&root) {
        Err(Failure::Integrity(_)) => {}
        other => panic!("expected Integrity, got {other:?}"),
    }
}

#[test]
fn a_missing_counter_on_a_committed_store_fails_closed() {
    let root = temp_root("counter");
    let mut store = JournaledStore::open(&root).unwrap();
    store
        .commit(&[b"x".to_vec()], &[], CallerIndex::NONE, b"m".to_vec())
        .unwrap();
    drop(store);

    std::fs::remove_file(root.join("counter")).unwrap();
    match JournaledStore::open(&root) {
        Err(Failure::Integrity(_)) => {}
        other => panic!("expected Integrity (no sequence reuse), got {other:?}"),
    }
}

#[test]
fn orphans_and_temps_are_collected_on_open() {
    let root = temp_root("gc");
    let mut store = JournaledStore::open(&root).unwrap();
    store
        .commit(&[b"kept".to_vec()], &[], CallerIndex::NONE, b"m".to_vec())
        .unwrap();
    drop(store);

    // Litter: a stray temp and an unreferenced (fake) object.
    std::fs::write(root.join("objects").join("deadbeef.tmp"), b"junk").unwrap();
    std::fs::write(root.join("objects").join("ab".repeat(32)), b"junk").unwrap();

    let store = JournaledStore::open(&root).unwrap();
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
fn required_set_tracks_live_state_not_commit_history() {
    // The store's promise, and the one that makes open affordable: a commit
    // costs what it changed, and what the store keeps is what it holds. Index
    // nodes are kept alive by reachability from a root, so a rewrite orphans
    // the spine it superseded — admitting those nodes as *required* instead
    // would make every one of them permanent and the required set would grow
    // with the number of commits ever performed.
    let root = temp_root("required-tracks-live");
    let mut store = JournaledStore::open(&root).unwrap();

    // One caller index, rewritten in place 64 times. Live state is constant:
    // one entry, one object.
    let mut caller_root: Option<journal::index::ChildRef> = None;
    let mut previous: Option<[u8; 32]> = None;
    for round in 0..64u32 {
        let object = format!("value-{round}").into_bytes();
        let hash = journal::object_content_hash(&object);
        let mut sink = journal::index::NodeSink::default();
        caller_root = journal::index::apply(
            &store.nodes(),
            caller_root,
            vec![journal::index::IndexChange {
                key: [7u8; 32],
                value: Some(hash.to_vec()),
            }],
            &mut sink,
        )
        .unwrap();
        let removed: Vec<[u8; 32]> = previous.into_iter().collect();
        let roots: Vec<journal::index::ChildRef> = caller_root.into_iter().collect();
        store
            .commit(
                &[object],
                &removed,
                CallerIndex {
                    roots: &roots,
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
    let reopened = JournaledStore::open(&root).unwrap();
    assert_eq!(reopened.required_objects().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_dir(root.join("objects")).unwrap().count(),
        objects,
        "an in-session sweep leaves nothing for the restart to find"
    );
}
