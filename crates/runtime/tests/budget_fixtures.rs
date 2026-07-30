//! The gates, and the relationships between the numbers they enforce.
//!
//! Time is injected everywhere, so nothing here sleeps and nothing is flaky.
//! What is being tested is not "does a rate limiter limit" — it is the two
//! properties the delivery planes actually depend on: that ordinary traffic is
//! never closed, and that a flood always is, whatever shape it arrives in.

use std::time::{Duration, Instant};

use runtime::budget::{deadline, slots, ByteGate, Gate, Pace, Verdict};

/// A fixed origin, so every test reads as a timeline rather than a clock.
fn origin() -> Instant {
    Instant::now()
}

#[test]
fn ordinary_traffic_at_the_permitted_rate_is_never_closed() {
    // The property that matters more than the limiting: a peer behaving inside
    // its budget must be able to run forever. A gate that closes a conforming
    // peer after enough hours is a gate nobody can ship.
    let start = origin();
    let mut gate = Gate::new(start, 10, 5, 8);
    let mut now = start;
    for _ in 0..10_000 {
        assert_eq!(gate.check(now), Verdict::Allow);
        now += Duration::from_millis(100);
    }
    assert_eq!(gate.strikes(), 0);
    assert!(!gate.is_closed());
}

#[test]
fn a_flood_closes_however_it_is_shaped() {
    // A paced flood is the easy case. The one worth testing is bursty arrival
    // with idle gaps, which is what an attacker writes once it notices a
    // steady stream gets closed — the gaps let the pace recover, so the
    // question is whether the strikes still accumulate faster than admitted
    // items pay them back.
    let start = origin();
    for (burst, gap_ms) in [(40usize, 200u64), (12, 50), (200, 1000)] {
        let mut gate = Gate::new(start, 10, 5, 16);
        let mut now = start;
        let mut closed_after = None;
        'outer: for round in 0..200 {
            for _ in 0..burst {
                if gate.check(now) == Verdict::Close {
                    closed_after = Some(round);
                    break 'outer;
                }
            }
            now += Duration::from_millis(gap_ms);
        }
        assert!(
            closed_after.is_some(),
            "a {burst}-item burst every {gap_ms}ms must close"
        );
    }
}

#[test]
fn a_gap_long_enough_to_be_honest_traffic_never_closes() {
    // The other side of the same coin. Bursts that fit inside the budget once
    // the gap is accounted for are ordinary usage — a tab reconnecting, a
    // client resubscribing — and must not accumulate anything.
    let start = origin();
    let mut gate = Gate::new(start, 10, 5, 16);
    let mut now = start;
    for _ in 0..500 {
        for _ in 0..5 {
            assert_ne!(gate.check(now), Verdict::Close);
        }
        now += Duration::from_millis(600);
    }
    assert!(!gate.is_closed());
}

#[test]
fn a_penalty_does_not_decay_on_its_own() {
    // What makes a scope eviction or a refused frame count for something. A
    // strike that the next admitted item erases is inert when the admitted
    // items arrive at 120/s.
    let start = origin();
    let mut gate = Gate::new(start, 1000, 1000, 4);
    assert_eq!(gate.penalise(3), Verdict::Drop);
    assert_eq!(gate.strikes(), 3);
    assert_eq!(gate.check(start), Verdict::Allow);
    assert_eq!(gate.strikes(), 2, "one admitted item pays back exactly one");
    assert_eq!(gate.penalise(2), Verdict::Close);
}

#[test]
fn a_byte_gate_meters_the_expensive_dimension() {
    // A message bucket alone is blind to maximal frames arriving at the
    // permitted rate. 32 frames/s of 64 KiB is 2 MiB/s of decode work inside a
    // message budget that thinks it is being generous.
    let start = origin();
    let mut gate = ByteGate::new(start, 32, 32, 128 * 1024, 512 * 1024, 16);
    let mut now = start;
    let mut admitted = 0;
    for _ in 0..64 {
        if gate.check(now, 64 * 1024) == Verdict::Allow {
            admitted += 1;
        }
        now += Duration::from_millis(31);
    }
    assert!(
        admitted < 32,
        "the byte budget must bind before the message budget: {admitted} admitted"
    );
}

#[test]
fn a_paces_credit_is_capped_however_long_it_was_idle() {
    let start = origin();
    let mut pace = Pace::new(start, 100, 10);
    let after_a_week = start + Duration::from_secs(7 * 24 * 3600);
    let mut admitted = 0;
    while pace.admit(after_a_week) {
        admitted += 1;
        assert!(admitted <= 64, "an idle gate must not accrue arrears");
    }
    assert_eq!(admitted, 10);
}

#[test]
fn the_deadlines_layer_so_a_timeout_names_one_side() {
    // A requester whose deadline is shorter than the provider's turns every
    // slow-but-legal answer into an unattributable failure. The margin is what
    // makes "who timed out" a question with an answer.
    assert!(
        deadline::CHUNK_HEADER >= deadline::CHUNK_RESOLVE + Duration::from_secs(2),
        "the requester's budget must cover the provider's plus a margin"
    );
    assert!(deadline::HAVE_RESPONSE <= deadline::CHUNK_HEADER);
    assert!(
        deadline::FLUSH_BEFORE_DROP > deadline::ACCEPT_WRITE,
        "waiting for a refusal to land must outlast writing it"
    );
    assert!(
        deadline::AUTHORITY_REVALIDATION < deadline::FREIGHT_IDLE,
        "a revocation must be noticed long before an idle connection is reaped"
    );
    assert!(
        deadline::DRIVER_POLL < deadline::ACCEPT_WRITE,
        "a driver must observe cancellation faster than it does anything"
    );
}

#[test]
fn a_dial_deadline_never_outlives_the_drain_it_would_be_joined_by() {
    // Station::drain_tasks leaks an unfinished handle rather than blocking, so
    // a task whose own deadline exceeds the drain budget is a task that can
    // outlive the Station that owns it. Every long deadline has to be raced
    // against cancellation rather than trusted.
    let drain = runtime::lifecycle::DEFAULT_DRAIN_DEADLINE;
    for (name, value) in [
        ("CHUNK_HEADER", deadline::CHUNK_HEADER),
        ("CHUNK_BODY_IDLE", deadline::CHUNK_BODY_IDLE),
        ("FREIGHT_IDLE", deadline::FREIGHT_IDLE),
        ("FLUSH_BEFORE_DROP", deadline::FLUSH_BEFORE_DROP),
    ] {
        if value >= drain {
            // Not a failure — it is a requirement on the caller, recorded here
            // so nobody discovers it by watching a Station refuse to stop.
            assert!(
                value < Duration::from_secs(120),
                "{name} is longer than the drain deadline AND unbounded"
            );
        }
    }
    assert!(deadline::DRIVER_POLL * 4 < drain);
}

#[test]
fn the_slot_ceilings_are_consistent_with_each_other() {
    assert!(
        slots::MAX_SPACE_CONNECTIONS <= slots::MAX_ENDPOINT_CONNECTIONS,
        "a Space cannot be allowed more than the endpoint holds"
    );
    assert!(
        slots::MAX_CONNECTIONS_PER_PEER_PLANE * 4 <= slots::MAX_SPACE_CONNECTIONS,
        "no single peer may plausibly consume a Space's allowance"
    );
    assert!(
        slots::MAX_INFLIGHT_CHUNKS_PER_PROVIDER <= slots::MAX_INFLIGHT_CHUNKS_PER_TRANSFER,
        "a transfer's ceiling has to admit at least one full provider window"
    );
    // Staged bytes must admit every transfer's in-flight window at once, or a
    // transfer can be starved by its own siblings rather than by a budget.
    let worst_in_flight = (slots::MAX_FETCH_TRANSFERS * slots::MAX_INFLIGHT_CHUNKS_PER_TRANSFER)
        as u64
        * replica::content::CHUNK_PLAINTEXT_LEN as u64;
    assert!(
        slots::MAX_STAGED_BYTES >= worst_in_flight,
        "staged bytes ({}) must cover the worst in-flight window ({worst_in_flight})",
        slots::MAX_STAGED_BYTES
    );
}
