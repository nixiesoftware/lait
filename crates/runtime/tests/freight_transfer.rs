//! Transfer bookkeeping: what a caller can see, and what a dead transfer leaves.
//!
//! Acceptance 12 lives here — two concurrent fetches of one content must not
//! collect each other's staged bytes. S2 ships deliberately *without* operation
//! coalescing so that scenario is reachable through the public surface rather
//! than only through a unit test of a scheduler.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use replica::content::ContentRef;
use replica::journal::cache::{Lease, ResidentCache};
use runtime::transfer::{
    TransferError, TransferHandle, TransferRegistry, TransferState, MAX_COMPLETED, PROGRESS_TICK,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn cache(tag: &str) -> Arc<ResidentCache> {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-transfer-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Arc::new(ResidentCache::open(dir, 1 << 30).unwrap())
}

fn content(n: u8) -> ContentRef {
    ContentRef {
        content_id: [n; 32],
    }
}

#[test]
fn two_operations_over_one_content_do_not_collect_each_others_staged_bytes() {
    // Acceptance 12. A lease keyed by content id alone would let whichever
    // transfer finished first sweep the other's partials — which is precisely
    // why the hold is keyed by operation, and why this is worth proving through
    // the handle rather than asserting about the cache.
    let cache = cache("concurrent");
    let registry = Arc::new(TransferRegistry::new());
    let now = Instant::now();
    let target = content(1);

    let first = TransferHandle::new(registry.clone(), cache.clone(), [1u8; 16], target, now)
        .expect("registered");
    let second = TransferHandle::new(registry.clone(), cache.clone(), [2u8; 16], target, now)
        .expect("registered");

    // Both are partway through the same content.
    let entry = replica::journal::object_content_hash(b"chunk-0");
    cache.install(&entry, b"chunk-0", b"proof").unwrap();
    for op in [first.operation(), second.operation()] {
        cache.append_staged(&op, 0, 0, b"half a chunk").unwrap();
        cache.lease(&Lease::operation(op, entry)).unwrap();
    }
    assert_eq!(registry.live_operations().len(), 2);

    // The first finishes. Its own holds go and nothing else does.
    first.succeed(now);
    assert_eq!(
        cache.staged_len(&[2u8; 16], 0),
        12,
        "the second transfer's staged bytes survive the first's completion"
    );
    assert!(
        cache.is_held(&entry).unwrap(),
        "and the entry is still held by the second"
    );
    assert_eq!(
        registry.live_operations(),
        [[2u8; 16]].into_iter().collect()
    );

    second.finish(TransferState::Cancelled, now);
    assert!(!cache.is_held(&entry).unwrap());
    assert!(registry.live_operations().is_empty());
}

#[test]
fn a_dropped_transfer_fails_itself_and_lets_go() {
    // A fetch task that panics, is cancelled, or returns early through a `?`
    // would otherwise leave an operation lease pinning its chunks and a staging
    // slot holding its bytes, with nothing to ever say the operation was over.
    let cache = cache("dropped");
    let registry = Arc::new(TransferRegistry::new());
    let now = Instant::now();
    let entry = replica::journal::object_content_hash(b"orphan");
    cache.install(&entry, b"orphan", b"proof").unwrap();

    {
        let handle =
            TransferHandle::new(registry.clone(), cache.clone(), [7u8; 16], content(2), now)
                .expect("registered");
        cache
            .append_staged(&handle.operation(), 0, 0, b"partial")
            .unwrap();
        cache
            .lease(&Lease::operation(handle.operation(), entry))
            .unwrap();
        handle.advance(
            TransferState::Transferring {
                bytes: 7,
                total: None,
            },
            now,
        );
        assert!(cache.is_held(&entry).unwrap());
    }

    assert_eq!(
        registry.state_of(&[7u8; 16]),
        Some(TransferState::Failed),
        "a transfer that just stopped existing did not succeed"
    );
    assert!(!cache.is_held(&entry).unwrap());
    assert_eq!(cache.staged_bytes(), 0);
    assert!(registry.live_operations().is_empty());
}

#[test]
fn a_state_change_publishes_at_once_and_byte_counts_coalesce() {
    // Progress is monotone, so coalescing loses nothing. A state change is the
    // thing a caller is usually waiting for, so it must not wait for a tick.
    let cache = cache("coalesce");
    let registry = Arc::new(TransferRegistry::new());
    let start = Instant::now();
    let handle = TransferHandle::new(
        registry.clone(),
        cache.clone(),
        [3u8; 16],
        content(3),
        start,
    )
    .expect("registered");
    let mut watch = registry.subscribe();
    let after_begin = *watch.borrow_and_update();

    handle.advance(TransferState::Connecting, start);
    assert_ne!(
        *watch.borrow_and_update(),
        after_begin,
        "a state change is published immediately"
    );

    let version = *watch.borrow();
    for byte in 1..50u64 {
        handle.advance(
            TransferState::Transferring {
                bytes: byte,
                total: Some(50),
            },
            start + Duration::from_millis(byte),
        );
    }
    // The first Transferring is a state change and publishes; the rest coalesce.
    let after_bytes = *watch.borrow();
    assert!(
        after_bytes - version <= 1,
        "forty-nine byte updates inside one tick published {} times",
        after_bytes - version
    );

    handle.advance(
        TransferState::Transferring {
            bytes: 50,
            total: Some(50),
        },
        start + PROGRESS_TICK * 2,
    );
    assert!(
        *watch.borrow() > after_bytes,
        "and a tick later it publishes"
    );

    handle.succeed(start + PROGRESS_TICK * 3);
    assert_eq!(
        registry.state_of(&[3u8; 16]),
        Some(TransferState::Available)
    );
    assert!(registry.active().is_empty());
}

#[test]
fn the_completed_tail_is_bounded_and_keeps_the_recent_end() {
    // Completed entries exist so a caller that asked a moment ago can find out
    // how it went. They are not a history, and one entry per completed transfer
    // would be a memory leak with a respectable name.
    let cache = cache("bounded");
    let registry = Arc::new(TransferRegistry::new());
    let now = Instant::now();
    for n in 0..(MAX_COMPLETED + 16) {
        let mut op = [0u8; 16];
        op[..8].copy_from_slice(&(n as u64).to_be_bytes());
        let handle = TransferHandle::new(registry.clone(), cache.clone(), op, content(4), now)
            .expect("registered");
        handle.succeed(now);
    }
    let completed = registry.completed();
    assert_eq!(completed.len(), MAX_COMPLETED);
    let last = completed.last().expect("a tail");
    assert_eq!(
        u64::from_be_bytes(last.operation[..8].try_into().unwrap()),
        (MAX_COMPLETED + 15) as u64,
        "the recent end is the end that is kept"
    );
}

#[test]
fn a_second_handle_for_a_live_operation_is_refused() {
    // Replacing looks harmless and is not: the displaced handle's Drop still
    // runs, and it releases the *new* transfer's leases and deletes its staged
    // bytes, because both are keyed by the same operation id. Silent data loss
    // with no failure anywhere to point at.
    let cache = cache("duplicate");
    let registry = Arc::new(TransferRegistry::new());
    let now = Instant::now();
    let first = TransferHandle::new(registry.clone(), cache.clone(), [4u8; 16], content(5), now)
        .expect("registered");

    assert_eq!(
        TransferHandle::new(registry.clone(), cache.clone(), [4u8; 16], content(5), now).err(),
        Some(TransferError::DuplicateOperation)
    );

    // And once the first is done, the id is free again.
    first.succeed(now);
    assert!(
        TransferHandle::new(registry.clone(), cache.clone(), [4u8; 16], content(5), now).is_ok()
    );
}

#[test]
fn the_active_set_is_bounded_because_a_cache_sweep_reads_it() {
    // `live_operations` is what says which staging is still wanted. An
    // unbounded active set is therefore not just memory — it is disk that is
    // never reclaimed.
    let cache = cache("active-bound");
    let registry = Arc::new(TransferRegistry::new());
    let now = Instant::now();
    let mut held = Vec::new();
    for n in 0..runtime::transfer::MAX_ACTIVE {
        let mut op = [0u8; 16];
        op[..8].copy_from_slice(&(n as u64).to_be_bytes());
        held.push(
            TransferHandle::new(registry.clone(), cache.clone(), op, content(6), now)
                .expect("inside the ceiling"),
        );
    }
    assert_eq!(
        TransferHandle::new(registry.clone(), cache.clone(), [0xFF; 16], content(6), now).err(),
        Some(TransferError::TooManyActive)
    );
    assert_eq!(
        registry.live_operations().len(),
        runtime::transfer::MAX_ACTIVE
    );
}
