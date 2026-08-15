//! Allocation traffic observed by the product's unit-test process.
//!
//! This wraps the same system allocator the test binary would otherwise use.
//! Counting is thread-local and opt-in, so unrelated tests and corpus setup do
//! not enter a sample. The wrapper exists only under `cfg(test)` and therefore
//! cannot change a shipped World implementation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingSystem;

#[global_allocator]
static ALLOCATOR: CountingSystem = CountingSystem;

fn record(bytes: usize) {
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
            let _ = CALLS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(1))
            });
            let _ = BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(bytes))
            });
        }
    });
}

// SAFETY: every operation delegates to `System` with the exact pointer and
// layout supplied by the caller. Observation uses only atomics and the
// allocation-free thread-local facility, never unwinds, and does not touch the
// returned allocation.
unsafe impl GlobalAlloc for CountingSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: this method inherits `GlobalAlloc::alloc`'s layout contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: this method inherits `GlobalAlloc::alloc_zeroed`'s contract.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: this method forwards the caller's live allocation unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: this method inherits `GlobalAlloc::realloc`'s pointer, layout,
        // and non-zero `new_size` contract.
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() {
            record(new_size);
        }
        resized
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(crate) struct Sample {
    pub(crate) calls: u64,
    pub(crate) bytes: u64,
    pub(crate) wall_micros: u128,
}

struct Active(bool);

impl Drop for Active {
    fn drop(&mut self) {
        let previous = self.0;
        let _ = ACTIVE.try_with(|active| active.set(previous));
    }
}

/// Measure gross successful allocation calls/bytes and elapsed wall time for
/// one synchronous operation on the calling thread.
pub(crate) fn measure<T>(operation: impl FnOnce() -> T) -> (T, Sample) {
    CALLS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    let previous = ACTIVE.with(|active| active.replace(true));
    let guard = Active(previous);
    let started = Instant::now();
    let value = operation();
    let elapsed: Duration = started.elapsed();
    drop(guard);
    (
        value,
        Sample {
            calls: CALLS.load(Ordering::Relaxed),
            bytes: BYTES.load(Ordering::Relaxed),
            wall_micros: elapsed.as_micros(),
        },
    )
}
