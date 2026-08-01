//! Behaviour that only shows up over time, tested without waiting for it.
//!
//! The runtime drivers now take their clock from `tokio::time::Instant` rather
//! than `std::time::Instant`. Without the `test-util` feature those are the
//! same call, so nothing about production changed. With it — and `n0-future`
//! enables it for the whole workspace already — `tokio::time::pause()` freezes
//! every call site at once and `advance()` moves them together, so ninety
//! seconds of driver behaviour costs milliseconds of test.
//!
//! ## What this can and cannot reach yet
//!
//! `plane_driver::drive` needs a `PlaneContext`, a `ConnectionQueue` and a
//! `PlaneService` to run, so what is asserted here is the beat arithmetic
//! against the real constants and the real tokio timer — not the driver
//! function itself. That is a smaller claim than "the driver maintains on
//! schedule", and it is written down rather than implied.
//!
//! It is still worth having, because the claim it does check was previously
//! unchecked at any price: the ratio between `DRIVER_POLL` and
//! `MAINTENANCE_INTERVAL` is what the comment on the latter is about, and
//! nothing stopped someone retuning either one until the two met.
//!
//! Driving the whole `drive()` loop under a paused clock is the next step and
//! the reason the seam exists; the scaffolding in `freight_two_node` is the
//! place to start.

use std::time::Duration;
use tokio::time::Instant;

use crate::budget::deadline;

/// The driver's slow-beat interval. Kept in step with `plane_driver`'s private
/// constant by `the_maintenance_interval_matches_the_driver` below, so this
/// copy cannot drift into agreeing with a test and disagreeing with the code.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);

/// The poll fires constantly; maintenance rides a much slower beat.
///
/// `plane_driver` explains why: the poll exists so cancellation is never
/// missed, and "sweeping on every 25 ms tick would be a directory walk forty
/// times a second". That is a claim about a RATIO — 1200 polls per maintenance
/// — and checking it used to mean ninety seconds of wall clock, which is a
/// polite way of saying nobody checked it.
///
/// `start_paused` makes the runtime auto-advance to the next timer whenever it
/// has nothing else to do, so this loop runs its ninety simulated seconds
/// immediately and deterministically.
#[tokio::test(start_paused = true)]
async fn maintenance_rides_a_slow_beat_not_the_poll() {
    let started = Instant::now();
    let mut last_maintained = Instant::now();
    let mut polls = 0u32;
    let mut maintenances = 0u32;

    while started.elapsed() < Duration::from_secs(90) {
        tokio::time::sleep(deadline::DRIVER_POLL).await;
        polls += 1;
        if last_maintained.elapsed() >= MAINTENANCE_INTERVAL {
            last_maintained = Instant::now();
            maintenances += 1;
        }
    }

    // 90 s / 25 ms. Exact, not approximate — that is the whole point of a
    // paused clock, and an approximate assertion here would be tolerating
    // nondeterminism that no longer exists.
    assert_eq!(polls, 3600, "the poll should fire every DRIVER_POLL");
    assert_eq!(
        maintenances, 3,
        "maintenance should fire once per MAINTENANCE_INTERVAL, not once per poll"
    );
    assert_eq!(
        polls / maintenances,
        1200,
        "the ratio the driver's comment is about"
    );
}

/// The clock really is frozen: elapsed time comes from `advance`, not from the
/// wall. Without this, every assertion above could be passing because the test
/// happened to run fast rather than because time was controlled.
#[tokio::test(start_paused = true)]
async fn the_paused_clock_does_not_advance_on_its_own() {
    let start = Instant::now();
    // Real work, no timers: a paused clock must not move for it.
    let mut sink = 0u64;
    for i in 0..500_000u64 {
        sink = sink.wrapping_add(i);
    }
    assert_eq!(start.elapsed(), Duration::ZERO, "sink={sink}");

    tokio::time::advance(Duration::from_secs(3600)).await;
    assert_eq!(start.elapsed(), Duration::from_secs(3600));
}

/// A deadline computed from `Instant::now()` inside the code under test — the
/// case a `now` parameter cannot cover — expires exactly when the clock says.
#[tokio::test(start_paused = true)]
async fn an_internally_computed_deadline_expires_on_the_simulated_clock() {
    fn deadline_from_now(timeout: Duration) -> Instant {
        // Shaped like `session::next_timeout` and `contact_driver`: the code
        // asks the clock itself rather than being handed a `now`.
        Instant::now() + timeout
    }

    let deadline = deadline_from_now(Duration::from_secs(15));
    assert!(Instant::now() < deadline);

    tokio::time::advance(Duration::from_secs(14)).await;
    assert!(Instant::now() < deadline, "not yet");

    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        Instant::now() >= deadline,
        "expired exactly on the deadline"
    );
}

/// The local copy of `MAINTENANCE_INTERVAL` matches the driver's.
///
/// The constant is private to `plane_driver`, and a test that quietly holds a
/// different value from the code is worse than no test. This reads the source
/// so the two cannot diverge silently — if the constant moves, this fails and
/// names the new value.
#[test]
fn the_maintenance_interval_matches_the_driver() {
    let source = include_str!("../plane_driver.rs");
    let declaration = source
        .lines()
        .find(|line| line.contains("const MAINTENANCE_INTERVAL"))
        .expect("plane_driver declares MAINTENANCE_INTERVAL");
    assert!(
        declaration.contains("from_secs(30)"),
        "MAINTENANCE_INTERVAL changed to `{}` — update the copy in this module \
         and the counts in `maintenance_rides_a_slow_beat_not_the_poll`",
        declaration.trim()
    );
    assert_eq!(MAINTENANCE_INTERVAL, Duration::from_secs(30));
}
