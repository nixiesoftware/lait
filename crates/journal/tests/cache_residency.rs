//! The resident cache: install, read, corruption recovery, per-operation
//! leases, pins, staging, and quota-driven eviction.
//!
//! The property under test throughout is that residency is *not* integrity. A
//! required object going missing takes the store down on purpose. A resident
//! chunk going missing must only mean "fetch it again", and the two must not be
//! reachable through the same call.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use journal::cache::{CacheError, Lease, ResidentCache};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-cache-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn address(bytes: &[u8]) -> [u8; 32] {
    journal::object_content_hash(bytes)
}

fn operation(n: u8) -> [u8; 16] {
    [n; 16]
}

#[test]
fn an_installed_entry_reads_back_with_its_sidecar() {
    let cache = ResidentCache::open(temp_root("install"), 1 << 20).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    assert!(cache.is_resident(&entry));
    assert_eq!(cache.read(&entry).unwrap(), (bytes, b"proof".to_vec()));
}

#[test]
fn bytes_that_do_not_match_their_address_are_refused_at_the_door() {
    let cache = ResidentCache::open(temp_root("address"), 1 << 20).unwrap();
    assert_eq!(
        cache.install(&[7u8; 32], b"anything", b"proof"),
        Err(CacheError::Corrupt)
    );
    assert!(!cache.is_resident(&[7u8; 32]));
}

#[test]
fn an_absent_entry_is_not_resident_rather_than_an_integrity_failure() {
    // The whole reason this is a separate store. A missing required object
    // takes the journal down; a missing chunk is a fetch.
    let cache = ResidentCache::open(temp_root("absent"), 1 << 20).unwrap();
    assert_eq!(cache.read(&[1u8; 32]), Err(CacheError::NotResident));
}

#[test]
fn corruption_drops_the_entry_and_leaves_it_refetchable() {
    let root = temp_root("corrupt");
    let cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    std::fs::write(root.join("chunks").join(hex(&entry)), b"tampered!!").unwrap();
    assert_eq!(cache.read(&entry), Err(CacheError::Corrupt));
    // Dropped, so the next read is an honest miss rather than a repeated lie.
    assert!(!cache.is_resident(&entry));
    assert_eq!(cache.read(&entry), Err(CacheError::NotResident));

    // And it can simply be installed again.
    cache.install(&entry, &bytes, b"proof").unwrap();
    assert!(cache.is_resident(&entry));
}

#[test]
fn a_chunk_without_its_sidecar_is_not_advertisable() {
    // A cache entry counts as resident only when both halves are here: a chunk
    // whose proof is missing cannot be served, and must not look like one that
    // can.
    let root = temp_root("halfpair");
    let cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    std::fs::remove_file(root.join("sidecars").join(hex(&entry))).unwrap();
    assert!(!cache.is_resident(&entry));
    assert_eq!(cache.read(&entry), Err(CacheError::NotResident));
}

#[test]
fn an_interrupted_install_is_reclaimed_on_open() {
    let root = temp_root("interrupted");
    {
        let cache = ResidentCache::open(&root, 1 << 20).unwrap();
        let bytes = b"ciphertext".to_vec();
        let entry = address(&bytes);
        cache.install(&entry, &bytes, b"proof").unwrap();
        // Simulate the window between the two renames.
        std::fs::remove_file(root.join("sidecars").join(hex(&entry))).unwrap();
    }
    let cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let names: Vec<String> = std::fs::read_dir(root.join("chunks"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.is_empty(),
        "a half-installed pair survives: {names:?}"
    );
    let _ = cache;
}

#[test]
fn two_operations_holding_one_entry_both_have_to_release_it() {
    // The failure this prevents: keying leases by entry alone lets the first
    // transfer to finish collect the second transfer's bytes.
    let cache = ResidentCache::open(temp_root("leases"), 0).unwrap();
    let bytes = b"shared".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    let first = Lease {
        operation: operation(1),
        entry,
    };
    let second = Lease {
        operation: operation(2),
        entry,
    };
    cache.lease(&first).unwrap();
    cache.lease(&second).unwrap();

    cache.release(&first).unwrap();
    cache.sweep().unwrap();
    assert!(
        cache.is_resident(&entry),
        "one release must not collect what another operation still holds"
    );

    cache.release(&second).unwrap();
    let report = cache.sweep().unwrap();
    assert!(!cache.is_resident(&entry));
    assert_eq!(report.entries_removed, 1);
}

#[test]
fn taking_a_lease_twice_holds_it_once() {
    let cache = ResidentCache::open(temp_root("idempotent"), 0).unwrap();
    let bytes = b"once".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();
    let lease = Lease {
        operation: operation(1),
        entry,
    };
    cache.lease(&lease).unwrap();
    cache.lease(&lease).unwrap();
    cache.release(&lease).unwrap();
    cache.sweep().unwrap();
    assert!(!cache.is_resident(&entry), "one release was enough");
}

#[test]
fn a_crashed_operation_releases_every_lease_it_held() {
    // Lease names carry their operation precisely so this is possible after a
    // restart, with no side file to keep consistent.
    let cache = ResidentCache::open(temp_root("crashed"), 0).unwrap();
    let mut entries = Vec::new();
    for n in 0..5u8 {
        let bytes = vec![n; 32];
        let entry = address(&bytes);
        cache.install(&entry, &bytes, b"proof").unwrap();
        cache
            .lease(&Lease {
                operation: operation(9),
                entry,
            })
            .unwrap();
        entries.push(entry);
    }
    assert_eq!(cache.release_operation(&operation(9)).unwrap(), 5);
    cache.sweep().unwrap();
    for entry in entries {
        assert!(!cache.is_resident(&entry));
    }
}

#[test]
fn a_pin_survives_quota_pressure() {
    let cache = ResidentCache::open(temp_root("pin"), 0).unwrap();
    let pinned_bytes = b"keep me".to_vec();
    let pinned = address(&pinned_bytes);
    let loose_bytes = b"evict me".to_vec();
    let loose = address(&loose_bytes);
    cache.install(&pinned, &pinned_bytes, b"proof").unwrap();
    cache.install(&loose, &loose_bytes, b"proof").unwrap();
    cache.pin(&pinned).unwrap();

    cache.sweep().unwrap();
    assert!(cache.is_resident(&pinned));
    assert!(!cache.is_resident(&loose));

    cache.unpin(&pinned).unwrap();
    cache.sweep().unwrap();
    assert!(!cache.is_resident(&pinned));
}

#[test]
fn a_sweep_inside_quota_evicts_nothing() {
    // Quota-driven, not time-driven: an entry is not stale because it is old.
    let cache = ResidentCache::open(temp_root("under"), 1 << 30).unwrap();
    let bytes = b"resident".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();
    assert_eq!(cache.sweep().unwrap().entries_removed, 0);
    assert!(cache.is_resident(&entry));
}

#[test]
fn staged_bytes_are_never_advertised_and_resume_from_where_they_are() {
    let cache = ResidentCache::open(temp_root("staging"), 1 << 20).unwrap();
    let op = operation(3);
    assert_eq!(cache.append_staged(&op, 0, 0, b"hello ").unwrap(), 6);
    assert_eq!(cache.append_staged(&op, 0, 6, b"world").unwrap(), 11);
    assert_eq!(cache.read_staged(&op, 0).unwrap(), b"hello world");

    // A resumed transfer proves where it is rather than being trusted about it.
    assert_eq!(
        cache.append_staged(&op, 0, 3, b"nope"),
        Err(CacheError::Corrupt)
    );

    // Partial bytes are not an entry: nothing about them is resident.
    assert!(!cache.is_resident(&address(b"hello world")));
}

#[test]
fn a_cancelled_operation_leaves_nothing_durable() {
    let root = temp_root("cancel");
    let cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let op = operation(4);
    cache.append_staged(&op, 0, 0, b"partial").unwrap();
    cache.append_staged(&op, 1, 0, b"more").unwrap();
    assert_eq!(cache.discard_staged(&op).unwrap(), 2);
    assert_eq!(
        std::fs::read_dir(root.join("staging")).unwrap().count(),
        0,
        "a cancelled ingest leaves no reclaimable residue"
    );
}

#[test]
fn staging_for_a_dead_operation_is_swept() {
    let cache = ResidentCache::open(temp_root("deadstage"), 1 << 20).unwrap();
    cache.append_staged(&operation(5), 0, 0, b"alive").unwrap();
    cache.append_staged(&operation(6), 0, 0, b"dead").unwrap();

    let live: BTreeSet<[u8; 16]> = [operation(5)].into_iter().collect();
    let report = cache.sweep_staging(&live).unwrap();
    assert_eq!(report.staging_removed, 1);
    assert!(cache.read_staged(&operation(5), 0).is_ok());
    assert!(cache.read_staged(&operation(6), 0).is_err());
}

#[test]
fn a_range_read_returns_only_what_was_asked_for() {
    let cache = ResidentCache::open(temp_root("range"), 1 << 20).unwrap();
    let bytes = b"0123456789".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();
    assert_eq!(cache.read_range(&entry, 3, 4).unwrap(), b"3456");
    // Past the end truncates rather than erroring: the caller asked for what
    // exists, and got it.
    assert_eq!(cache.read_range(&entry, 8, 10).unwrap(), b"89");
    assert_eq!(
        cache.read_range(&[0u8; 32], 0, 1),
        Err(CacheError::NotResident)
    );
}

#[test]
fn eviction_reclaims_the_most_room_for_the_fewest_refetches() {
    let cache = ResidentCache::open(temp_root("largest"), 40).unwrap();
    let big = vec![1u8; 200];
    let small = vec![2u8; 10];
    cache.install(&address(&big), &big, b"p").unwrap();
    cache.install(&address(&small), &small, b"p").unwrap();

    cache.sweep().unwrap();
    assert!(!cache.is_resident(&address(&big)), "largest goes first");
    assert!(cache.is_resident(&address(&small)));
}

fn hex(hash: &[u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(hash)
}
