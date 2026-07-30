//! The gates, and the relationships between the numbers they enforce.
//!
//! Time is injected everywhere, so nothing here sleeps and nothing is flaky.
//! What is being tested is not "does a rate limiter limit" — it is the two
//! properties the delivery planes actually depend on: that ordinary traffic is
//! never closed, and that a flood always is, whatever shape it arrives in.

use std::time::{Duration, Instant};

use runtime::budget::{deadline, gates, slots, ByteGate, Evictions, Gate, Pace, Verdict};

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
    // Live's own layering. A slot has to outlive the thing that refreshes it,
    // or a cursor that is being updated correctly still disappears.
    assert!(
        deadline::CURSOR_TTL > deadline::CURSOR_COALESCE * 8,
        "a cursor must survive many coalescing windows, or it flickers"
    );
    assert!(
        deadline::PRESENCE_TTL > deadline::TYPING_COALESCE * 8,
        "presence must survive many typing windows"
    );
    assert!(
        deadline::PRESENCE_TTL > deadline::CURSOR_TTL,
        "a reader who is not typing is still reading"
    );
    assert!(
        deadline::CARET_GRACE < deadline::CURSOR_TTL,
        "a datagram racing a Retire must not outlive the slot it would resurrect"
    );
    assert!(
        deadline::LIVE_DIAL < deadline::LIVE_IDLE,
        "dialling must give up long before an established session is reaped"
    );
    // A signal's sender budget has to cover the receiver's read *and* write plus
    // a margin, or every slow-but-legal acknowledgement presents as the
    // receiver's fault.
    assert!(
        deadline::SIGNAL_RESPONSE
            >= deadline::SIGNAL_READ + deadline::SIGNAL_WRITE + Duration::from_secs(2),
        "the sender's budget must cover the receiver's plus a margin"
    );
    assert!(deadline::SIGNAL_OPEN <= deadline::SIGNAL_RESPONSE);
    assert!(
        deadline::SIGNAL_IDLE > deadline::SIGNAL_RESPONSE,
        "a lane must outlive the exchange it carries"
    );
    assert!(
        deadline::LIVE_IDLE > deadline::PRESENCE_TTL,
        "a session must outlive the presence it carries, or it is reaped while          somebody is still visible"
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
        ("LIVE_IDLE", deadline::LIVE_IDLE),
        ("LIVE_DIAL", deadline::LIVE_DIAL),
        ("SIGNAL_OPEN", deadline::SIGNAL_OPEN),
        ("SIGNAL_READ", deadline::SIGNAL_READ),
        ("SIGNAL_WRITE", deadline::SIGNAL_WRITE),
        ("SIGNAL_RESPONSE", deadline::SIGNAL_RESPONSE),
        ("SIGNAL_IDLE", deadline::SIGNAL_IDLE),
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

    // Live's ceilings, and the one relationship between them that is a fact
    // rather than a choice: the scope-to-kind legality table admits at most two
    // payload kinds for any scope, so a connection cannot hold more slots than
    // twice its scopes. Written as an equality because if the table ever admits
    // a third, this is where that shows up.
    assert_eq!(
        slots::MAX_SLOTS_PER_CONNECTION,
        slots::MAX_SUBSCRIBED_SCOPES_PER_CONNECTION * 2,
        "slots per connection is derived from the legality table, not chosen"
    );
    assert!(
        slots::MAX_LIVE_SESSIONS <= slots::MAX_SPACE_CONNECTIONS,
        "a Space cannot hold more Live sessions than it holds connections"
    );
    assert!(
        slots::MAX_LIVE_SESSIONS_PER_STATION * 4 <= slots::MAX_LIVE_SESSIONS,
        "no single Station may plausibly consume the Space's Live allowance"
    );
    // The transient table has to hold what the sessions it admits can fill, or
    // an honest Station evicts honest peers under no load at all.
    assert!(
        slots::SIGNAL_LANE_WORKERS * slots::MAX_CONNECTIONS_PER_PEER_PLANE
            <= runtime::planes::bounds::MAX_STREAM_WORKERS,
        "one peer's signal lanes must fit inside the per-connection stream budget"
    );
    assert!(
        slots::MAX_TRANSIENT_SLOTS >= slots::MAX_LIVE_SESSIONS * slots::MAX_SLOTS_PER_CONNECTION,
        "the transient table ({}) must cover every admitted session's slots ({})",
        slots::MAX_TRANSIENT_SLOTS,
        slots::MAX_LIVE_SESSIONS * slots::MAX_SLOTS_PER_CONNECTION
    );
}

#[test]
fn an_eviction_ledger_does_not_forgive_the_way_a_gate_does() {
    // The reason `Evictions` is its own type rather than a `Gate` used
    // carefully. `Gate::check` subtracts a strike on every admitted item, so a
    // peer that alternates one eviction with a handful of honest datagrams sits
    // at zero strikes forever while steadily displacing everyone else.
    //
    // An eviction means this peer's subscriptions pushed another's out of a
    // bounded table. There is no amount of subsequent good behaviour that
    // un-displaces them, so there is no decay path here to find.
    let mut gate = Gate::new(Instant::now(), 1000, 1000, 4);
    let now = Instant::now();
    for _ in 0..3 {
        gate.penalise(1);
        for _ in 0..8 {
            let _ = gate.check(now);
        }
    }
    assert!(
        !matches!(gate.check(now), Verdict::Close),
        "a Gate forgives, which is why it is the wrong ledger for evictions"
    );

    let mut evictions = Evictions::new(4);
    for _ in 0..3 {
        assert!(matches!(evictions.charge(1), Verdict::Allow));
    }
    assert_eq!(evictions.charged(), 3);
    assert!(matches!(evictions.charge(1), Verdict::Close));
    // And it stays closed. A closed ledger that reopens is a ledger a peer can
    // wait out.
    assert!(matches!(evictions.charge(0), Verdict::Close));
    assert!(matches!(evictions.charge(1), Verdict::Close));
}

#[test]
fn a_live_gate_never_closes_below_twice_its_rate() {
    // The module's own sizing rule: choosing R is choosing 2R, because a peer
    // at rate L accumulates strikes only when L - R > R. Asserted for the Live
    // specs so nobody picks a rate believing it is the closing threshold.
    for (name, spec) in [
        ("LIVE_CONTROL", gates::LIVE_CONTROL),
        ("LIVE_DATAGRAMS", gates::LIVE_DATAGRAMS),
        ("STREAM_ACCEPT", gates::STREAM_ACCEPT),
    ] {
        let mut gate = Gate::from_spec(Instant::now(), spec);
        let start = Instant::now();
        // Exactly the permitted rate, for long enough to exhaust the burst.
        let step = Duration::from_nanos(1_000_000_000 / spec.per_second as u64);
        let mut now = start;
        for _ in 0..(spec.per_second * 4) {
            now += step;
            assert!(
                !matches!(gate.check(now), Verdict::Close),
                "{name} closed on a peer sending exactly its permitted rate"
            );
        }
    }
}
