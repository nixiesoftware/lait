//! The migration fault matrix: a prior-layout store must arrive in the pack
//! verified whole and retire crash-idempotently, the source staying
//! authoritative until the seal — and every ambiguous aftermath must
//! fail-stop as a named divergence, never resolve silently.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{index, v1, Defect, Failure, Index, Manifest, Object, Store, FAULT_POINTS};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-migrate-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_object(root: &Path, bytes: &[u8]) -> Object {
    let hash = crate::object_content_hash(bytes);
    std::fs::write(root.join("objects").join(crate::hex(&hash)), bytes).unwrap();
    Object {
        hash,
        len: u64::try_from(bytes.len()).unwrap(),
    }
}

/// Write a complete, valid prior-layout store by hand: eager objects, their
/// index, caller meta, manifest, counter. `counter` may sit above the
/// manifest sequence — reserved-but-uncommitted history the migration must
/// never reissue.
fn install_v1_store(root: &Path, objects: &[&[u8]], meta: &[u8], counter: u64) -> Vec<Object> {
    std::fs::create_dir_all(root.join("objects")).unwrap();
    std::fs::create_dir_all(root.join("journal")).unwrap();
    let refs: Vec<Object> = objects
        .iter()
        .map(|bytes| write_object(root, bytes))
        .collect();
    let meta_ref = write_object(root, meta);
    let entries: Vec<index::IndexEntry> = refs
        .iter()
        .map(|object| {
            let mut value = vec![1u8];
            value.extend_from_slice(&object.len.to_be_bytes());
            index::IndexEntry {
                key: object.hash,
                value,
            }
        })
        .collect();
    let mut sink = index::NodeSink::default();
    let eager_root = index::build_index(entries, &mut sink).unwrap();
    for bytes in sink.written {
        write_object(root, &bytes);
    }
    let manifest = Manifest {
        format_version: crate::STORE_FORMAT_VERSION,
        sequence: 1,
        eager_object_index_root: eager_root.map(|r| (r.hash, r.count)),
        deferred_object_index_root: None,
        caller_meta: Some(meta_ref),
        caller_index_roots: Vec::new(),
        lazy_caller_index_roots: Vec::new(),
    };
    std::fs::write(
        root.join("current-manifest"),
        postcard::to_stdvec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("counter"), counter.to_le_bytes()).unwrap();
    refs
}

fn assert_migrated(root: &Path, meta: &[u8], refs: &[Object]) {
    let store = Store::open(root).unwrap();
    assert_eq!(store.caller_meta().unwrap().unwrap(), meta);
    for object in refs {
        assert_eq!(
            store.read_object(object).unwrap().len(),
            usize::try_from(object.len).unwrap()
        );
    }
    assert!(v1::tombstoned(root), "the tombstone stands");
    assert!(!root.join("objects").exists(), "the source moved aside");
    assert!(root.join("retired-v1").is_dir(), "…into retirement");
}

#[test]
fn a_prior_layout_store_migrates_verified_and_continues_the_sequence() {
    let root = temp_root("happy");
    let refs = install_v1_store(&root, &[b"alpha", b"beta"], b"the-meta", 9);

    assert_migrated(&root, b"the-meta", &refs);
    let mut store = Store::open(&root).unwrap();
    let next = store
        .commit(&[b"gamma".to_vec()], &[], Index::NONE, b"m2".to_vec())
        .unwrap();
    assert!(
        next > 10,
        "every sequence sits above the source's reserved high water: {next}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_crash_at_every_migration_point_leaves_one_authoritative_history() {
    for &point in &FAULT_POINTS {
        let root = temp_root(&format!("crash-{point}"));
        let refs = install_v1_store(&root, &[b"survives"], b"m", 3);

        let refused = Store::open_with_fault_injector(&root, Box::new(move |name| name == point));
        assert!(
            refused.is_err(),
            "{point}: the crashed open reports failure"
        );
        // Whatever the crash left — an unsealed pack (source authoritative,
        // remigrated) or a sealed one the abort never got to retire — the
        // next open converges on the one migrated history.
        assert_migrated(&root, b"m", &refs);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn an_interrupted_retirement_resumes_only_for_a_matching_source() {
    let root = temp_root("resume");
    let refs = install_v1_store(&root, &[b"kept"], b"m", 2);
    assert_migrated(&root, b"m", &refs);

    // Reconstruct the crash-before-retirement state: the source back in
    // place, the tombstone gone.
    let retired = root.join("retired-v1");
    std::fs::rename(retired.join("objects"), root.join("objects")).unwrap();
    std::fs::rename(retired.join("counter"), root.join("counter")).unwrap();
    std::fs::copy(
        retired.join("current-manifest"),
        root.join("current-manifest"),
    )
    .unwrap();

    assert_migrated(&root, b"m", &refs);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_source_mutated_after_sealing_is_a_divergence_not_a_retirement() {
    let root = temp_root("diverged");
    let refs = install_v1_store(&root, &[b"kept"], b"m", 2);
    assert_migrated(&root, b"m", &refs);

    // The dangerous interleaving: an old binary wrote to the source after
    // the pack sealed. The manifest no longer matches the provenance.
    std::fs::create_dir_all(root.join("objects")).unwrap();
    install_v1_store(&root, &[b"newer-history"], b"other-meta", 7);

    assert!(matches!(
        Store::open(&root),
        Err(Failure::Integrity(Defect::Diverged))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_tombstone_whose_pack_is_missing_is_a_divergence() {
    let root = temp_root("lost-pack");
    let refs = install_v1_store(&root, &[b"kept"], b"m", 2);
    assert_migrated(&root, b"m", &refs);

    for name in ["hot-0", "hot-1"] {
        let _ = std::fs::remove_file(root.join(name));
    }
    assert!(matches!(
        Store::open(&root),
        Err(Failure::Integrity(Defect::Diverged))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_never_committed_prior_layout_retires_into_a_fresh_pack() {
    let root = temp_root("empty-v1");
    std::fs::create_dir_all(root.join("objects")).unwrap();
    std::fs::create_dir_all(root.join("journal")).unwrap();

    let mut store = Store::open(&root).unwrap();
    assert!(store.manifest().is_none());
    store
        .commit(&[b"first".to_vec()], &[], Index::NONE, b"m".to_vec())
        .unwrap();
    drop(store);
    assert!(v1::tombstoned(&root));
    assert_eq!(
        Store::open(&root).unwrap().caller_meta().unwrap().unwrap(),
        b"m"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_tombstone_reads_as_unsupported_and_refuses_the_old_rebuild() {
    let bytes = v1::tombstone_bytes();
    // An old binary's open: unsupported, never corrupt — the message that
    // says rebuild-or-restore instead of summoning a damage response.
    assert!(matches!(
        crate::decode_manifest(&bytes),
        Err(Failure::Integrity(Defect::UnsupportedFormat))
    ));
    // An old binary's rebuild: the prior-generation reader must refuse it
    // outright rather than succeed as an empty store and fork history.
    let root = temp_root("tombstone-rebuild");
    std::fs::create_dir_all(root.join("objects")).unwrap();
    std::fs::write(root.join("current-manifest"), &bytes).unwrap();
    assert!(
        crate::GenerationSource::open(&root).is_err(),
        "a tombstone must never rebuild as an empty prior store"
    );
    let _ = std::fs::remove_dir_all(&root);
}
