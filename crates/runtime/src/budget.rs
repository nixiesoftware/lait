//! Paces, gates, and the deadlines the delivery planes run against.
//!
//! Deliberately not in [`crate::planes::bounds`], whose contract is one thing:
//! a pre-allocation ceiling, checked against a declared length before a buffer
//! is reserved. A rate is a different kind of number — it bounds behaviour over
//! time rather than one allocation — and mixing them would make "is this
//! checked before we allocate?" a question you have to look up.
//!
//! **Why a virtual-time pace rather than a refilled bucket.** A token bucket
//! needs its tokens put back, which means either a timer per gate or a
//! divide-on-every-check. GCRA keeps one `Instant` — the theoretical arrival
//! time of the next conforming item — and answers by comparing against it. No
//! refiller to stall, no state proportional to the burst, and the same
//! admission decisions.
//!
//! **Why strikes decay on admit rather than on a timer.** The shape is
//! Rayfish's: a token bucket plus a strike counter, where every admitted
//! message decays a strike. A chatty peer that occasionally overshoots is
//! therefore never closed — its own conforming traffic pays its strikes back —
//! while a sustained flood, which by definition admits nothing, accumulates
//! them. A timer-based decay would let a peer flood in bursts spaced to the
//! timer and never close.
//!
//! One gate per connection-owning task. Nothing here is `Sync`, nothing takes a
//! lock, and that is the point: the hot path is a comparison and an add.

use std::time::{Duration, Instant};

/// What a gate decided about one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Within budget. Handle it.
    Allow,
    /// Over budget. Discard this item; the peer stays.
    Drop,
    /// Sustained over budget. Close the connection.
    Close,
}

/// A rate limiter as a single deadline.
///
/// `interval` is the minimum spacing between conforming items, and `burst` is
/// how far ahead of that spacing an arrival may run. A caller that has been
/// quiet accumulates credit up to `burst` and no further, so an idle hour buys
/// one burst rather than an hour of arrears.
#[derive(Debug, Clone)]
pub struct Pace {
    /// The theoretical arrival time of the next conforming item.
    next: Instant,
    interval: Duration,
    burst: Duration,
}

impl Pace {
    /// `per_second` items sustained, `burst` items of head start.
    ///
    /// A zero or absurd rate would make `interval` meaningless, so both are
    /// clamped rather than trusted: a gate misconfigured to zero should throttle
    /// hard, not divide by zero or admit everything.
    pub fn new(now: Instant, per_second: u32, burst: u32) -> Self {
        let per_second = per_second.clamp(1, 1_000_000);
        let interval = Duration::from_nanos(1_000_000_000 / per_second as u64);
        Self {
            next: now,
            interval,
            // A burst of N means N items admitted back to back, so the
            // tolerance is the N-1 intervals they arrive ahead of schedule.
            burst: interval * burst.max(1).saturating_sub(1),
        }
    }

    /// Whether one item conforms, consuming its share if it does.
    pub fn admit(&mut self, now: Instant) -> bool {
        self.admit_cost(now, 1)
    }

    /// Whether `cost` items' worth conforms. A byte gate is the same algorithm
    /// with bytes as the unit, so it shares the implementation rather than
    /// growing a parallel one.
    pub fn admit_cost(&mut self, now: Instant, cost: u32) -> bool {
        let cost = self.interval.saturating_mul(cost.max(1));
        // Credit is capped at `burst`: an idle gate does not accrue arrears.
        let theoretical = self.next.max(now);
        if theoretical.checked_sub(self.burst).is_some_and(|e| e > now) {
            return false;
        }
        self.next = self.next.max(now).checked_add(cost).unwrap_or(self.next);
        true
    }

    /// Whether `cost` would conform, without consuming anything. What a paired
    /// gate uses so a rejected item is charged to neither half.
    pub fn would_admit(&self, now: Instant, cost: u32) -> bool {
        let _ = cost;
        !self
            .next
            .max(now)
            .checked_sub(self.burst)
            .is_some_and(|earliest| earliest > now)
    }
}

/// A pace plus the strike ledger that turns sustained abuse into a close.
///
/// **`Close` latches.** Once a gate has closed it stays closed, which is lait's
/// addition to the cited shape rather than part of it. A caller that drains its
/// remaining work before tearing the connection down would otherwise watch the
/// gate reopen as time passed and have to remember, separately, that it had
/// already decided to close.
#[derive(Debug, Clone)]
pub struct Gate {
    pace: Pace,
    strikes: u16,
    strike_limit: u16,
    closed: bool,
}

impl Gate {
    pub fn new(now: Instant, per_second: u32, burst: u32, strike_limit: u16) -> Self {
        Self {
            pace: Pace::new(now, per_second, burst),
            strikes: 0,
            strike_limit: strike_limit.max(1),
            closed: false,
        }
    }

    pub fn check(&mut self, now: Instant) -> Verdict {
        if self.closed {
            return Verdict::Close;
        }
        if self.pace.admit(now) {
            self.strikes = self.strikes.saturating_sub(1);
            return Verdict::Allow;
        }
        self.strikes = self.strikes.saturating_add(1);
        if self.strikes >= self.strike_limit {
            self.closed = true;
            return Verdict::Close;
        }
        Verdict::Drop
    }

    /// Charge a strike for something the pace cannot see — an evicted scope, a
    /// refused frame. Never decays on its own; only an admitted item pays it
    /// back.
    pub fn penalise(&mut self, count: u16) -> Verdict {
        if self.closed {
            return Verdict::Close;
        }
        self.strikes = self.strikes.saturating_add(count);
        if self.strikes >= self.strike_limit {
            self.closed = true;
            return Verdict::Close;
        }
        Verdict::Drop
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn strikes(&self) -> u16 {
        self.strikes
    }
}

/// Two paces over the same stream: how many, and how large.
///
/// Both or neither. A message bucket alone is blind to a peer sending maximal
/// frames at the permitted rate, and a byte bucket alone is blind to a flood of
/// empty ones — but charging one when the other refuses would let a peer
/// exhaust the cheap budget to starve the expensive one.
#[derive(Debug, Clone)]
pub struct ByteGate {
    messages: Pace,
    bytes: Pace,
    strikes: u16,
    strike_limit: u16,
    closed: bool,
}

impl ByteGate {
    pub fn new(
        now: Instant,
        messages_per_second: u32,
        message_burst: u32,
        bytes_per_second: u32,
        byte_burst: u32,
        strike_limit: u16,
    ) -> Self {
        Self {
            messages: Pace::new(now, messages_per_second, message_burst),
            bytes: Pace::new(now, bytes_per_second, byte_burst),
            strikes: 0,
            strike_limit: strike_limit.max(1),
            closed: false,
        }
    }

    pub fn check(&mut self, now: Instant, len: usize) -> Verdict {
        if self.closed {
            return Verdict::Close;
        }
        let cost = u32::try_from(len).unwrap_or(u32::MAX).max(1);
        if self.messages.would_admit(now, 1) && self.bytes.would_admit(now, cost) {
            self.messages.admit(now);
            self.bytes.admit_cost(now, cost);
            self.strikes = self.strikes.saturating_sub(1);
            return Verdict::Allow;
        }
        self.strikes = self.strikes.saturating_add(1);
        if self.strikes >= self.strike_limit {
            self.closed = true;
            return Verdict::Close;
        }
        Verdict::Drop
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// The deadlines the planes run against.
///
/// Every one bounds how long a peer can hold something of ours. They are
/// layered on purpose: a requester's deadline exceeds the provider's by a
/// margin, so a timeout is attributable to one side rather than to a race.
pub mod deadline {
    use std::time::Duration;

    /// Writing the accept. Longer than this and the peer is not reading.
    pub const ACCEPT_WRITE: Duration = Duration::from_secs(2);

    /// After a one-shot write, wait this long for the peer to have it.
    ///
    /// Dropping a connection resets its streams, so a coarse refusal that is
    /// written and immediately dropped reaches the peer as an ambiguous
    /// transport error it will retry — which is the opposite of a refusal.
    pub const FLUSH_BEFORE_DROP: Duration = Duration::from_secs(5);

    /// A provider's budget to resolve a descriptor and answer.
    pub const CHUNK_RESOLVE: Duration = Duration::from_secs(5);

    /// A requester's budget for the same exchange, covering the provider's plus
    /// a margin so a timeout names one side.
    pub const CHUNK_HEADER: Duration = Duration::from_secs(8);

    /// Bulk bytes are the one part whose legitimate duration is unbounded, so
    /// the bound is on *progress*: the timer resets only on a non-empty read.
    pub const CHUNK_BODY_IDLE: Duration = Duration::from_secs(10);

    /// A unary availability answer. A provider never scans to produce one, so
    /// this bounds a stall rather than a legitimate search.
    pub const HAVE_RESPONSE: Duration = Duration::from_secs(5);

    /// A Freight connection with no transfer in flight still holds an endpoint
    /// slot, a Space slot, and a per-peer slot.
    pub const FREIGHT_IDLE: Duration = Duration::from_secs(60);

    /// How often a driver re-examines whether its peers are still authorized.
    pub const AUTHORITY_REVALIDATION: Duration = Duration::from_secs(2);

    /// A driver's poll interval, so cancellation is never missed while parked.
    pub const DRIVER_POLL: Duration = Duration::from_millis(25);
}

/// Counting permits and slot ceilings.
///
/// These are concurrency bounds, not rates: a permit is held while work is in
/// flight and released when it ends, so the number answers "how much can be
/// happening at once" rather than "how fast may it start". Shared levels get
/// permits and no strike ledger — a strike ledger at a shared level would let
/// one hostile member evict every honest one.
pub mod slots {
    /// Inbound connections one identity endpoint will hold.
    ///
    /// Two ALPNs run on one endpoint, so a single peer may legitimately hold a
    /// Freight connection and a Live connection at once.
    pub const MAX_ENDPOINT_CONNECTIONS: usize = 128;

    /// Inbound connections one Space will hold. Half the endpoint ceiling, so
    /// no Space can starve a sibling — the numeric form of the isolation the
    /// hub already provides structurally.
    pub const MAX_SPACE_CONNECTIONS: usize = 64;

    /// Inbound connections one peer may hold on one plane. One reconnect may
    /// legitimately overlap one connection that is still closing; more than
    /// that is how a single member consumes a Space's whole allowance.
    pub const MAX_CONNECTIONS_PER_PEER_PLANE: usize = 2;

    /// Concurrent serve tasks per Space.
    ///
    /// Acquired *before* the task is spawned. Acquiring inside would let a
    /// flood outrun the cap by queueing tasks, which is a bound on nothing.
    pub const MAX_SERVE_WORKERS: usize = 32;

    /// Concurrent inbound transfers per Space.
    pub const MAX_FETCH_TRANSFERS: usize = 8;

    /// Chunks in flight to one provider. Four 256 KiB chunks keeps an ~80 Mbit/s
    /// path busy at 100 ms of round trip; more is buffer, not throughput.
    pub const MAX_INFLIGHT_CHUNKS_PER_PROVIDER: usize = 4;

    /// Chunks in flight for one transfer, across every provider.
    ///
    /// Low on purpose. A partially transferred chunk cannot be verified, cannot
    /// be served, and is lost if the transfer dies — so finishing chunks beats
    /// starting them.
    pub const MAX_INFLIGHT_CHUNKS_PER_TRANSFER: usize = 8;

    /// Staged bytes per Space.
    ///
    /// Staging is real disk that the cache quota does not count: an entry is
    /// not resident until it installs. Without its own ceiling a fleet of
    /// half-finished transfers fills a disk while the cache reports itself
    /// comfortably inside its quota.
    pub const MAX_STAGED_BYTES: u64 = 64 * 1024 * 1024;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pace_admits_its_burst_then_meters() {
        let start = Instant::now();
        let mut pace = Pace::new(start, 10, 5);
        for _ in 0..5 {
            assert!(pace.admit(start));
        }
        assert!(!pace.admit(start), "the burst is spent");
        assert!(pace.admit(start + Duration::from_millis(100)));
    }

    #[test]
    fn an_idle_pace_buys_one_burst_not_an_hour_of_arrears() {
        let start = Instant::now();
        let mut pace = Pace::new(start, 10, 5);
        let later = start + Duration::from_secs(3600);
        for _ in 0..5 {
            assert!(pace.admit(later));
        }
        assert!(!pace.admit(later), "credit is capped at the burst");
    }

    #[test]
    fn a_chatty_peer_is_never_closed_and_a_flood_always_is() {
        let start = Instant::now();
        let mut gate = Gate::new(start, 10, 2, 4);
        // Overshoot, then behave. The strikes are paid back by admitted items.
        let mut now = start;
        for _ in 0..20 {
            gate.check(now); // one over the burst
            gate.check(now);
            gate.check(now);
            now += Duration::from_millis(300);
        }
        assert!(!gate.is_closed(), "occasional overshoot is not abuse");

        let mut flood = Gate::new(start, 10, 2, 4);
        let mut verdict = Verdict::Allow;
        for _ in 0..64 {
            verdict = flood.check(start);
        }
        assert_eq!(verdict, Verdict::Close);
        assert_eq!(
            flood.check(start + Duration::from_secs(60)),
            Verdict::Close,
            "close latches"
        );
    }

    #[test]
    fn a_byte_gate_charges_both_halves_or_neither() {
        let start = Instant::now();
        // One message per second, but only 100 bytes per second.
        let mut gate = ByteGate::new(start, 100, 10, 100, 100, 8);
        assert_eq!(gate.check(start, 100), Verdict::Allow);
        assert_eq!(gate.check(start, 100), Verdict::Drop, "bytes are spent");
        // The refused item must not have consumed a message slot either.
        assert_eq!(
            gate.check(start + Duration::from_secs(2), 1),
            Verdict::Allow
        );
    }
}
