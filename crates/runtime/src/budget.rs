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
//! therefore never closed — its own conforming traffic pays its strikes back.
//!
//! That gives an exact never-close ceiling, and it is worth stating rather than
//! discovering: a peer arriving at rate λ against a permitted rate R has R
//! admitted and λ−R denied per second, so strikes accumulate only while
//! λ − R > R — that is, **above twice the permitted rate**. Everything between
//! R and 2R is throttled forever and never closed, which is the intended
//! answer for a peer that is merely enthusiastic. Sizing R is therefore sizing
//! 2R, and every gate below is chosen with that in mind. A timer-based decay
//! would have no such ceiling: a peer could flood in bursts spaced to the timer
//! and never close at any rate.
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
        // No upper clamp. A byte pace is a rate in *bytes* per second, so eight
        // million is an ordinary number here — clamping it at a million would
        // silently throttle every byte lane to under a megabyte a second, and
        // the symptom would be "the network is slow" rather than "a constant is
        // wrong". The quotient is floored at one nanosecond instead, which is
        // the only value that actually breaks the arithmetic.
        let per_second = per_second.max(1);
        let interval = Duration::from_nanos((1_000_000_000 / per_second as u64).max(1));
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
        let charge = self.interval.saturating_mul(cost.max(1));
        // Compared forward, never by subtracting from an `Instant`. On Windows
        // an `Instant` is time since boot, so `next - burst` underflows during
        // the first seconds of uptime — and a `checked_sub` returning `None`
        // read as "conforming" would hand out an extra burst exactly then.
        if self.next.max(now) > now + self.burst {
            return false;
        }
        // Failing closed on overflow. Admitting an item and charging nothing
        // for it is the one outcome a rate limiter must never have.
        let Some(next) = self.next.max(now).checked_add(charge) else {
            return false;
        };
        self.next = next;
        true
    }

    /// Whether `cost` would conform, without consuming anything. What a paired
    /// gate uses so a rejected item is charged to neither half.
    ///
    /// Weighted like [`Self::admit_cost`]: asking whether one *byte* fits and
    /// then charging for sixty-four kilobytes would let a large item through a
    /// gate that has room for a small one.
    pub fn would_admit(&self, now: Instant, cost: u32) -> bool {
        let charge = self.interval.saturating_mul(cost.max(1));
        self.next.max(now).checked_add(charge).is_some() && self.next.max(now) <= now + self.burst
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
    /// Build from a named spec. The door a call site should use.
    pub fn from_spec(now: Instant, spec: gates::GateSpec) -> Self {
        Self::new(now, spec.per_second, spec.burst, spec.strike_limit)
    }

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
        if count == 0 {
            // Charging nothing is not a denial. A caller sweeping on a quiet
            // tick would otherwise drop an item for no reason.
            return Verdict::Allow;
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
    /// Build from a named spec.
    pub fn from_spec(now: Instant, spec: gates::ByteGateSpec) -> Self {
        Self::new(
            now,
            spec.messages_per_second,
            spec.message_burst,
            spec.bytes_per_second,
            spec.byte_burst,
            spec.strike_limit,
        )
    }

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

    /// A Live session with no traffic at all.
    ///
    /// Longer than Freight's, because a person reading an issue emits nothing
    /// for a while and is still there — but bounded just above
    /// [`PRESENCE_TTL`], because past that point their presence has already
    /// expired and the session is holding a connection to say nothing about
    /// somebody nobody can see. They reconnect the moment they do anything.
    ///
    /// It also has to stay under the ceiling `budget_fixtures` enforces on any
    /// deadline longer than the drain budget: a task that outlives the drain is
    /// a task that outlives its Station, so a long deadline is only safe when
    /// it is raced against cancellation, and the fixture is what stops one
    /// being added on the assumption that it is.
    pub const LIVE_IDLE: Duration = Duration::from_secs(100);

    /// Dialling a peer's Live plane. Short: a Station that does not answer
    /// promptly is one whose cursors nobody is waiting for.
    pub const LIVE_DIAL: Duration = Duration::from_secs(5);

    /// How long a caret is held before it is sent.
    ///
    /// A caret moves as fast as a person types and is superseded by its own
    /// next position, so sending each one costs a packet to deliver a number
    /// that is already wrong. Below the threshold where a remote cursor stops
    /// feeling live.
    pub const CURSOR_COALESCE: Duration = Duration::from_millis(80);

    /// How long typing is held before it is sent. Coarser than a caret,
    /// because "someone is typing" has no intermediate values worth sending.
    pub const TYPING_COALESCE: Duration = Duration::from_millis(500);

    /// How long a cursor survives without an update.
    ///
    /// This is what makes a crashed tab disappear rather than leaving a ghost
    /// caret in a document forever. Transient state has no goodbye it can rely
    /// on, so every slot expires on its own.
    pub const CURSOR_TTL: Duration = Duration::from_secs(30);

    /// How long presence survives without an update. Longer than a cursor: a
    /// reader who is not typing is still reading.
    pub const PRESENCE_TTL: Duration = Duration::from_secs(90);

    /// How long a caret survives a `Retire` that may have raced a datagram
    /// already in flight. Below every TTL, so a raced item cannot outlive the
    /// slot it would resurrect.
    pub const CARET_GRACE: Duration = Duration::from_secs(2);

    /// Opening the stream a signal rides. One message per stream, so this is
    /// the whole handshake budget.
    pub const SIGNAL_OPEN: Duration = Duration::from_secs(5);

    /// Reading one signal off its stream.
    pub const SIGNAL_READ: Duration = Duration::from_secs(5);

    /// Writing one signal onto its stream.
    pub const SIGNAL_WRITE: Duration = Duration::from_secs(5);

    /// The sender's whole budget for a signal that expects an acknowledgement.
    ///
    /// Layered above the receiver's read and write so a timeout names one side:
    /// if the sender's budget were the tighter one, every slow-but-legal
    /// acknowledgement would present as the receiver's fault.
    pub const SIGNAL_RESPONSE: Duration = Duration::from_secs(20);

    /// A signal lane with nothing on it.
    pub const SIGNAL_IDLE: Duration = Duration::from_secs(60);

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

/// The gates, by name.
///
/// A call site that constructs a gate from literals is a call site that can
/// pick the wrong table — and the first one to do it did, using the live
/// plane's byte budget for Freight, where the docket says that budget never
/// applies. Naming them here means a fixture can see them and a reviewer can
/// compare them.
pub mod gates {
    /// How many, how large, and how much abuse before closing.
    #[derive(Debug, Clone, Copy)]
    pub struct GateSpec {
        pub per_second: u32,
        pub burst: u32,
        pub strike_limit: u16,
    }

    /// A paced count paired with a paced volume.
    #[derive(Debug, Clone, Copy)]
    pub struct ByteGateSpec {
        pub messages_per_second: u32,
        pub message_burst: u32,
        pub bytes_per_second: u32,
        pub byte_burst: u32,
        pub strike_limit: u16,
    }

    /// Freight requests per connection.
    ///
    /// A request is one flow carrying one bounded frame; the expensive part is
    /// what it asks for, which the byte gate meters separately. 64/s sustained
    /// means an honest fetcher pulling 256 KiB chunks saturates a gigabit link
    /// long before it saturates this.
    pub const FREIGHT_REQUESTS: GateSpec = GateSpec {
        per_second: 64,
        burst: 256,
        strike_limit: 128,
    };

    /// Freight bytes served per connection.
    ///
    /// Deliberately generous: this is bulk transfer, and the whole point of the
    /// plane is that it moves files. It bounds a peer that asks for everything
    /// at once, not one that asks steadily.
    pub const FREIGHT_BYTES: ByteGateSpec = ByteGateSpec {
        messages_per_second: 64,
        message_burst: 256,
        bytes_per_second: 8 * 1024 * 1024,
        byte_burst: 32 * 1024 * 1024,
        strike_limit: 128,
    };

    /// Live control messages per connection.
    ///
    /// Control is subscribe and retire — a person opening and closing views, so
    /// a handful per second sustained with room for a burst when a tab opens
    /// several at once. Far tighter than Freight because a control message
    /// costs a subscription table entry rather than bytes off a disk.
    pub const LIVE_CONTROL: GateSpec = GateSpec {
        per_second: 16,
        burst: 64,
        strike_limit: 64,
    };

    /// Live control bytes per connection.
    ///
    /// A subscription names scopes, so its size is bounded by the scope ceiling
    /// rather than by anything a peer chooses freely. This exists to bound the
    /// peer that sends maximal messages at the maximal rate, which the message
    /// gate alone does not.
    pub const LIVE_CONTROL_BYTES: ByteGateSpec = ByteGateSpec {
        messages_per_second: 16,
        message_burst: 64,
        bytes_per_second: 256 * 1024,
        byte_burst: 1024 * 1024,
        strike_limit: 64,
    };

    /// Live datagrams per connection.
    ///
    /// A caret moves as fast as a person types; presence and typing coalesce on
    /// their own deadlines. 64/s sustained is well above what any honest client
    /// emits after coalescing, and the burst absorbs a reconnect replaying
    /// what it holds.
    ///
    /// A gate never closes below twice its rate — see this module's opening
    /// note — so choosing 64 is choosing "closes above 128/s, sustained".
    pub const LIVE_DATAGRAMS: GateSpec = GateSpec {
        per_second: 64,
        burst: 256,
        strike_limit: 128,
    };

    /// Live datagram bytes per connection.
    ///
    /// Small on purpose. Every transient item is bounded individually, so this
    /// bounds the aggregate — and the aggregate is the number that decides
    /// whether one member can make a Station spend its uplink on cursors.
    pub const LIVE_DATAGRAM_BYTES: ByteGateSpec = ByteGateSpec {
        messages_per_second: 64,
        message_burst: 256,
        bytes_per_second: 512 * 1024,
        byte_burst: 2 * 1024 * 1024,
        strike_limit: 128,
    };

    /// Reliable signals accepted on one connection.
    ///
    /// A signal is a person-scale event — an invitation, a file offer, an
    /// attention ping. Nothing honest emits them quickly, so this is tight, and
    /// tight is what makes it a bound worth having: an unbounded signal rate is
    /// a member who can make every peer's Station do bounded work forever.
    pub const SIGNAL_RATE: GateSpec = GateSpec {
        per_second: 4,
        burst: 16,
        strike_limit: 32,
    };

    /// Reliable signal bytes accepted on one connection.
    ///
    /// Each signal is individually bounded, so this bounds the aggregate — and
    /// the aggregate is what decides whether a member can spend a Station's
    /// uplink on invitations nobody asked for.
    pub const SIGNAL_BYTES: ByteGateSpec = ByteGateSpec {
        messages_per_second: 4,
        message_burst: 16,
        bytes_per_second: 64 * 1024,
        byte_burst: 256 * 1024,
        strike_limit: 32,
    };

    /// New streams accepted on one connection.
    ///
    /// A stream is cheap to open and not cheap to serve, which is the shape of
    /// every accept-side flood. Separate from the message gates because a peer
    /// that opens streams and sends nothing on them never reaches those.
    pub const STREAM_ACCEPT: GateSpec = GateSpec {
        per_second: 32,
        burst: 128,
        strike_limit: 64,
    };
}

/// A ledger that counts and never forgives.
///
/// [`Gate`] decays: every admitted message subtracts a strike, which is right
/// for a peer whose behaviour is mostly fine and occasionally bursty. It is
/// wrong for eviction. An eviction means this peer's own subscriptions pushed
/// another's out of a bounded table, and a peer that alternates one eviction
/// with eight honest datagrams would sit at zero strikes forever while steadily
/// displacing everyone else — because `Gate::check` decrements on every admit
/// and `penalise` shares that same counter.
///
/// So this is its own type rather than a `Gate` used carefully. There is no
/// decay path, and there is no way to add one without noticing.
#[derive(Debug, Clone)]
pub struct Evictions {
    charged: u32,
    limit: u32,
}

impl Evictions {
    pub fn new(limit: u32) -> Self {
        Self {
            charged: 0,
            limit: limit.max(1),
        }
    }

    /// Charge `n` evictions. `Close` once the limit is reached, and every time
    /// after — a closed ledger stays closed.
    pub fn charge(&mut self, n: u32) -> Verdict {
        self.charged = self.charged.saturating_add(n);
        if self.charged >= self.limit {
            Verdict::Close
        } else {
            Verdict::Allow
        }
    }

    pub fn charged(&self) -> u32 {
        self.charged
    }
}

/// The relationships between the slot ceilings, checked when this file is
/// compiled rather than when a test is run.
///
/// These were fixtures. They are stronger here: a ceiling changed to something
/// inconsistent stops the build, and stops it at the file where the number
/// lives, instead of failing a test somebody has to go and read. Each carries
/// the sentence that says what the relationship is *for* — which is the part a
/// bare comparison would lose.
mod consistency {
    use super::slots;

    const _: () = assert!(
        slots::MAX_SPACE_CONNECTIONS <= slots::MAX_ENDPOINT_CONNECTIONS,
        "a Space cannot be allowed more than the endpoint holds"
    );
    const _: () = assert!(
        slots::MAX_CONNECTIONS_PER_PEER_PLANE * 4 <= slots::MAX_SPACE_CONNECTIONS,
        "no single peer may plausibly consume a Space's whole allowance"
    );
    const _: () = assert!(
        slots::MAX_INFLIGHT_CHUNKS_PER_PROVIDER <= slots::MAX_INFLIGHT_CHUNKS_PER_TRANSFER,
        "a transfer's ceiling has to admit at least one full provider window"
    );
    const _: () = assert!(
        slots::MAX_LIVE_SESSIONS <= slots::MAX_SPACE_CONNECTIONS,
        "a Space cannot hold more Live sessions than it holds connections"
    );
    const _: () = assert!(
        slots::MAX_LIVE_SESSIONS_PER_STATION * 4 <= slots::MAX_LIVE_SESSIONS,
        "no single Station may plausibly consume the Space's Live allowance"
    );
    const _: () = assert!(
        slots::MAX_SLOTS_PER_CONNECTION == slots::MAX_SUBSCRIBED_SCOPES_PER_CONNECTION * 2,
        "slots per connection is derived from the legality table, not chosen:          no scope admits more than two payload kinds"
    );
    const _: () = assert!(
        slots::MAX_TRANSIENT_SLOTS
            >= slots::MAX_LIVE_SESSIONS * slots::MAX_SLOTS_PER_CONNECTION,
        "the transient table must cover every admitted session's slots, or an          honest Station evicts honest peers under no load at all"
    );
    const _: () = assert!(
        slots::SIGNAL_LANE_WORKERS * slots::MAX_CONNECTIONS_PER_PEER_PLANE
            <= crate::planes::bounds::MAX_STREAM_WORKERS,
        "one peer's signal lanes must fit inside the per-connection stream budget"
    );
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

    /// Concurrent signal-lane workers per connection.
    ///
    /// A signal is one bounded message on its own short stream, so this bounds
    /// how many a peer may have in flight rather than how many it may send.
    pub const SIGNAL_LANE_WORKERS: usize = 4;

    /// Live sessions one Station will hold.
    ///
    /// Below the Space connection ceiling, because a Live session costs a
    /// subscription table and a slot budget that a bare connection does not.
    pub const MAX_LIVE_SESSIONS: usize = 32;

    /// Live sessions one peer Station may hold. One reconnect may overlap one
    /// session still closing; more is a member consuming the Space's share.
    pub const MAX_LIVE_SESSIONS_PER_STATION: usize = 2;

    /// Outbound Live dials in flight at once.
    pub const MAX_LIVE_DIALS_IN_FLIGHT: usize = 4;

    /// Transient slots one Station will hold across every session.
    ///
    /// The whole transient table. Nothing in it is durable and all of it is
    /// evictable, so this bounds memory rather than correctness — but an
    /// unbounded table is a Station a Space can make allocate without ever
    /// committing anything.
    pub const MAX_TRANSIENT_SLOTS: usize = 4096;

    /// Scopes one connection may subscribe to.
    ///
    /// A person has a handful of views open. This is generous enough that no
    /// honest client meets it and tight enough that a hostile one cannot make
    /// the subscription table the expensive part.
    pub const MAX_SUBSCRIBED_SCOPES_PER_CONNECTION: usize = 64;

    /// Transient slots one connection may occupy.
    ///
    /// Exactly twice the scope ceiling, and that is a fact rather than a
    /// choice: the legality table admits at most two payload kinds per scope
    /// (a text caret takes `Caret` or `Selection`; every other scope takes
    /// one). A connection cannot hold more slots than its scopes allow.
    pub const MAX_SLOTS_PER_CONNECTION: usize = MAX_SUBSCRIBED_SCOPES_PER_CONNECTION * 2;

    /// Evictions one connection may cause before it is closed.
    ///
    /// Small, and deliberately not derived from the table size. A peer causing
    /// eight evictions has displaced eight other people's state from a shared
    /// table, and the ninth is not a busier peer — it is a peer whose traffic
    /// pattern only makes sense as displacement. The first eviction is nobody's
    /// fault; the eighth is nobody else's.
    pub const MAX_EVICTIONS_PER_CONNECTION: u32 = 8;

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
