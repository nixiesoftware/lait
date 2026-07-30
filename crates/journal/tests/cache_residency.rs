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
fn a_slot_returns_exactly_what_was_put_in_it() {
    // The slot is the caller's name and this cache does not interpret it. What
    // it does promise is that the bytes come back byte-identical — the address
    // travels *with* them rather than being the filename, so tampering is still
    // caught and still drops the entry.
    let root = temp_root("address");
    let cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let slot = [7u8; 32];
    cache.install(&slot, b"anything", b"proof").unwrap();
    assert_eq!(
        cache.read(&slot).unwrap(),
        (b"anything".to_vec(), b"proof".to_vec())
    );

    let path = root.join("chunks").join(hex(&slot));
    let mut raw = std::fs::read(&path).unwrap();
    *raw.last_mut().unwrap() ^= 0xFF;
    std::fs::write(&path, &raw).unwrap();
    assert_eq!(cache.read(&slot), Err(CacheError::Corrupt));
    assert!(!cache.is_resident(&slot), "corrupt bytes are dropped");
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
fn an_entry_cannot_be_half_present() {
    // The state this cache must not be able to reach. Bytes and sidecar are one
    // file published by one rename, so there is no window in which a chunk
    // exists without the proof that makes it servable.
    let root = temp_root("halfpair");
    let cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    let files: Vec<String> = std::fs::read_dir(root.join("chunks"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, vec![hex(&entry)], "one file per entry");

    // A truncated entry file is corrupt rather than half-resident, and reading
    // it drops it so the next attempt refetches.
    std::fs::write(root.join("chunks").join(hex(&entry)), b"	").unwrap();
    assert_eq!(cache.read(&entry), Err(CacheError::Corrupt));
    assert!(!cache.is_resident(&entry));
}

#[test]
fn an_interrupted_install_is_reclaimed_on_open() {
    let root = temp_root("interrupted");
    {
        let cache = ResidentCache::open(&root, 1 << 20).unwrap();
        let bytes = b"ciphertext".to_vec();
        let entry = address(&bytes);
        cache.install(&entry, &bytes, b"proof").unwrap();
        // What a crash before the rename actually leaves behind.
        std::fs::write(
            root.join("chunks").join(format!("{}.tmp", hex(&entry))),
            b"x",
        )
        .unwrap();
    }
    let _cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let names: Vec<String> = std::fs::read_dir(root.join("chunks"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|n| n.ends_with(".tmp")),
        "a temporary survives: {names:?}"
    );
    assert_eq!(names.len(), 1, "and the installed entry is untouched");
}

#[test]
fn two_operations_holding_one_entry_both_have_to_release_it() {
    // The failure this prevents: keying leases by entry alone lets the first
    // transfer to finish collect the second transfer's bytes.
    let cache = ResidentCache::open(temp_root("leases"), 0).unwrap();
    let bytes = b"shared".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    let first = Lease::operation(operation(1), entry);
    let second = Lease::operation(operation(2), entry);
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
    let lease = Lease::operation(operation(1), entry);
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
        cache.lease(&Lease::operation(operation(9), entry)).unwrap();
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
    // The quota has to sit above one small entry and below the pair, or the
    // test is not about eviction order.
    let cache = ResidentCache::open(temp_root("largest"), 128).unwrap();
    let big = vec![1u8; 400];
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

#[test]
fn an_operation_release_cannot_drop_a_content_hold() {
    // Both holders are sixteen bytes, and a content nonce is a public field of
    // a replicated descriptor. Only the kind keeps them apart, so a caller that
    // released "the operation whose id happens to equal this nonce" would be
    // dropping bytes some other holder committed.
    let cache = ResidentCache::open(temp_root("kinds"), 1 << 20).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    let shared = operation(5);
    cache.lease(&Lease::content(shared, entry)).unwrap();
    assert_eq!(cache.release_operation(&shared).unwrap(), 0);
    assert!(cache.is_held(&entry).unwrap(), "the content hold survives");

    assert_eq!(cache.release_content(&shared).unwrap(), 1);
    assert!(!cache.is_held(&entry).unwrap());
}

#[test]
fn a_sweep_that_cannot_reach_the_quota_says_so() {
    // Every chunk of a committed content is held by that content's own lease,
    // so a cache full of committed content has no eligible victims at all. The
    // sweep must not report success while sitting far over quota — an operator
    // reading "0 reclaimed, Ok" learns nothing about needing to forget
    // something.
    let cache = ResidentCache::open(temp_root("shortfall"), 16).unwrap();
    let mut resident = 0u64;
    for n in 0..4u8 {
        let bytes = vec![n; 4096];
        let entry = address(&bytes);
        cache.install(&entry, &bytes, b"proof").unwrap();
        cache.lease(&Lease::content([7u8; 16], entry)).unwrap();
        resident += 1;
    }
    assert_eq!(resident, 4);

    let report = cache.sweep().unwrap();
    assert_eq!(report.entries_removed, 0);
    assert!(
        report.over_quota_bytes > 16_000,
        "the shortfall must be reported: {report:?}"
    );

    // Forgetting the content makes them evictable, and then the quota is met.
    cache.release_content(&[7u8; 16]).unwrap();
    let report = cache.sweep().unwrap();
    assert_eq!(report.entries_removed, 4);
    assert_eq!(report.over_quota_bytes, 0);
}

#[test]
fn an_unreadable_tag_directory_stops_the_sweep_rather_than_freeing_everything() {
    // The one read that decides whether deletion is allowed. Answering
    // "nothing is held" on an I/O error would make every pinned and leased
    // entry collectable at exactly the moment the filesystem is unwell.
    let root = temp_root("tags-gone");
    let cache = ResidentCache::open(&root, 0).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();
    cache.pin(&entry).unwrap();

    std::fs::remove_dir_all(root.join("tags")).unwrap();
    assert!(
        matches!(cache.sweep(), Err(CacheError::Durability(_))),
        "a sweep that cannot read the holds must refuse"
    );
    assert!(cache.is_resident(&entry), "and must not have deleted");
    assert!(matches!(
        cache.evict(&entry),
        Err(CacheError::Durability(_))
    ));
    assert!(cache.is_resident(&entry));
}

#[test]
fn a_transient_read_failure_does_not_delete_the_entry() {
    // Corrupt means "these bytes are wrong", and dropping is the right answer.
    // A read that failed for any other reason is not evidence of anything, and
    // deleting on it turns a busy filesystem into data loss.
    let root = temp_root("transient");
    let cache = ResidentCache::open(&root, 1 << 20).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();

    // A directory where the entry file should be: reads fail, but not because
    // the bytes are wrong.
    let path = root.join("chunks").join(hex(&entry));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(matches!(cache.read(&entry), Err(CacheError::Durability(_))));
    assert!(path.exists(), "a transient failure must not delete");
}

#[test]
fn finishing_one_staged_part_leaves_the_others_alone() {
    // `discard_staged` is prefix-matched over the whole operation, which is
    // right for a cancelled transfer and catastrophic for a finished chunk: a
    // transfer that installed its third chunk and then discarded the operation
    // would delete the partials for every other chunk still in flight.
    let cache = ResidentCache::open(temp_root("staged-part"), 1 << 20).unwrap();
    let op = operation(3);
    for part in 0..4u32 {
        cache
            .append_staged(&op, part, 0, &vec![part as u8; 100])
            .unwrap();
    }
    assert_eq!(cache.staged_bytes(), 400);
    assert_eq!(cache.staged_len(&op, 2), 100);

    cache.discard_staged_part(&op, 2).unwrap();
    assert_eq!(cache.staged_len(&op, 2), 0);
    assert_eq!(cache.staged_bytes(), 300, "only part 2 went");
    for part in [0u32, 1, 3] {
        assert_eq!(cache.read_staged(&op, part).unwrap().len(), 100);
    }

    // Discarding a part that is already gone is not an error — a retry after a
    // crash between install and discard must be able to finish.
    cache.discard_staged_part(&op, 2).unwrap();

    // And the whole-operation door still works.
    cache.discard_staged(&op).unwrap();
    assert_eq!(cache.staged_bytes(), 0);
}

#[test]
fn a_dead_operations_lease_is_released_and_a_content_hold_is_not() {
    // An operation lease outlives the process that took it, which is what lets
    // an interrupted transfer resume. The cost is that a crashed transfer holds
    // its chunks forever unless someone says the operation is over — and only
    // the caller knows which are still live.
    let cache = ResidentCache::open(temp_root("sweep-leases"), 1 << 20).unwrap();
    let bytes = b"ciphertext".to_vec();
    let entry = address(&bytes);
    cache.install(&entry, &bytes, b"proof").unwrap();
    cache.lease(&Lease::operation(operation(1), entry)).unwrap();
    cache.lease(&Lease::operation(operation(2), entry)).unwrap();
    cache.lease(&Lease::content([9u8; 16], entry)).unwrap();

    let live = BTreeSet::from([operation(1)]);
    assert_eq!(
        cache.sweep_leases(&live).unwrap(),
        1,
        "only operation 2 was dead"
    );
    assert!(cache.is_held(&entry).unwrap());

    assert_eq!(cache.sweep_leases(&BTreeSet::new()).unwrap(), 1);
    assert!(
        cache.is_held(&entry).unwrap(),
        "a content hold belongs to committed content, and no restart makes it stale"
    );
    cache.release_content(&[9u8; 16]).unwrap();
    assert!(!cache.is_held(&entry).unwrap());
}
