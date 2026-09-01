//! Wall-clock time, in one place, with a seam for tests.
//!
//! ## Why this exists
//!
//! `tokio::time::pause` controls `Instant` — the monotonic clock, which is what
//! deadlines, beats and timeouts are measured against. It does **not** control
//! `SystemTime`; tokio has an open issue for it and no mock. So the wall clock,
//! which is what goes into records, needed its own seam or none at all.
//!
//! It also existed four times. `src/app.rs`, `src/orbital/hosting.rs`,
//! `src/orbital/mechanics.rs` and `products/issues-app/src/host.rs` each carried
//! a private `now_secs()` that did the same `duration_since(UNIX_EPOCH)` and the
//! same `unwrap_or(0)`. Four copies of a function is four places for a decision
//! about the pre-epoch case to drift.
//!
//! ## The seam
//!
//! A frozen-at value in an atomic, checked before the real clock — the same
//! shape tokio uses for its own paused clock, and for the same reason: the
//! branch has to be present in every build so callers do not need a feature
//! flag, and a relaxed load next to a syscall does not register.
//!
//! It is a process-global, which is safe here for a specific reason: nextest
//! runs every test in its own process, so two tests cannot fight over it. Under
//! plain `cargo test` — which this workspace uses only for doctests — they
//! could, and [`Frozen`] would be the wrong tool.
//!
//! ## What this is not
//!
//! Not for ULIDs. [`crate::ids::UlidSource`] already abstracts the millisecond
//! source that identifier minting uses, and a test that needs deterministic ids
//! passes its own — that seam predates this one and is the better shape where a
//! caller can accept a parameter. This is for the callers that cannot: free
//! functions producing a timestamp for a record.

use std::sync::atomic::{AtomicU64, Ordering};

/// Sentinel for "not frozen". `u64::MAX` seconds is about 585 billion years
/// past the epoch, so it cannot collide with a real reading.
const RUNNING: u64 = u64::MAX;

static FROZEN_MILLIS: AtomicU64 = AtomicU64::new(RUNNING);

#[cfg(target_arch = "wasm32")]
fn real_millis() -> u64 {
    // web-time asks the JS host; std's SystemTime panics in a browser, and a
    // panic inside a timestamp is a worse lie than any clock.
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn real_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        // A clock before the epoch, or past u64 milliseconds, is not a case any
        // caller can do anything useful with. Zero is the one value every
        // consumer here already treats as "unknown", and it was what all four
        // copies of this function returned.
        .unwrap_or(0)
}

/// Milliseconds since the unix epoch.
pub fn now_millis() -> u64 {
    match FROZEN_MILLIS.load(Ordering::Relaxed) {
        RUNNING => real_millis(),
        frozen => frozen,
    }
}

/// Seconds since the unix epoch.
pub fn now_secs() -> u64 {
    now_millis() / 1_000
}

/// A frozen wall clock, restored when dropped.
///
/// Tests only. There is no production caller and there should never be one:
/// freezing the wall clock in a running daemon would make every record it
/// writes claim the same instant.
#[must_use = "the clock unfreezes when this guard is dropped"]
pub struct Frozen(());

impl Frozen {
    /// Freeze the wall clock at `millis` until the guard drops.
    pub fn at_millis(millis: u64) -> Self {
        assert_ne!(millis, RUNNING, "u64::MAX is the not-frozen sentinel");
        FROZEN_MILLIS.store(millis, Ordering::Relaxed);
        Self(())
    }

    /// Move a frozen clock forward. Panics if the clock is not frozen, because
    /// advancing a running wall clock is not a thing that can be done.
    pub fn advance_millis(&self, millis: u64) {
        let current = FROZEN_MILLIS.load(Ordering::Relaxed);
        assert_ne!(current, RUNNING, "the clock is not frozen");
        FROZEN_MILLIS.store(current.saturating_add(millis), Ordering::Relaxed);
    }
}

impl Drop for Frozen {
    fn drop(&mut self) {
        FROZEN_MILLIS.store(RUNNING, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_clock_reports_the_real_time() {
        let before = real_millis();
        let seen = now_millis();
        assert!(
            seen >= before,
            "an unfrozen clock reads the real one: {seen} < {before}"
        );
    }

    #[test]
    fn a_frozen_clock_does_not_move_and_thaws_on_drop() {
        {
            let clock = Frozen::at_millis(1_700_000_000_000);
            assert_eq!(now_millis(), 1_700_000_000_000);
            assert_eq!(now_secs(), 1_700_000_000);
            clock.advance_millis(5_000);
            assert_eq!(now_secs(), 1_700_000_005);
        }
        // The guard is gone, so the real clock is back. Asserted against a
        // known-past timestamp rather than a range: any real reading is far
        // beyond the frozen one, and this cannot be flaky.
        assert!(now_millis() > 1_700_000_005_000);
    }

    #[test]
    fn seconds_are_milliseconds_divided_not_rounded() {
        let _clock = Frozen::at_millis(1_999);
        assert_eq!(now_secs(), 1, "999 ms is not another second");
    }
}
