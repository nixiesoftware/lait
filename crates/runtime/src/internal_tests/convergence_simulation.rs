//! Deterministic multi-peer convergence simulation.
//!
//! `two_node_convergence` proves the pipeline: one peer exports signed
//! material, it survives framing and validation, the other incorporates it,
//! and the two agree. That is the mechanism working on a good day, with two
//! peers, one transaction, and a network that delivers.
//!
//! This is the other question. Four peers author concurrently, the network
//! drops, duplicates, reorders and partitions, and the simulation asks whether
//! the fleet still ends up in one state — and in the RIGHT one.
//!
//! ## Why a seed rather than a fixture
//!
//! A convergence bug is almost never in the path someone wrote a test for. It
//! is in an interleaving: this transaction arriving before that one, at this
//! peer, while that peer was partitioned. There are too many interleavings to
//! enumerate and no reason to think a hand-picked one is the bad one. So the
//! schedule is generated from a seed.
//!
//! ## The seed replays the run, and that is measured
//!
//! It did not at first, and finding out took measuring rather than reading.
//! The schedule came from a running generator, which couples every later
//! decision to how many draws came before it — and that count is NOT a function
//! of the seed here. `gossip` draws once per exported item, and
//! `Replica::export_material` yields a different number of items on each run,
//! because it groups material by transaction-commitment hash and those
//! commitments embed OS entropy: sealing nonces, minted content ids, FROST's
//! own `OsRng`. Two runs of one seed stayed identical for sixteen steps and
//! then parted.
//!
//! Seeding Fabric's identity minting was tried and does not fix it — there is
//! no single door at this layer, which is exactly why madsim intercepts
//! `getrandom` process-wide instead of seaming call sites. That seam was built,
//! measured, and taken back out rather than left in a security-sensitive path
//! earning nothing.
//!
//! What fixes it is [`Schedule`]: decisions drawn from `(seed, step, purpose,
//! index)` rather than from a running generator, so each is independent of
//! every other. The system underneath may still produce different material on
//! two runs; which step commits what, and which envelope slot drops, is the
//! same either way. `a_seed_replays_its_run` asserts it, and seeds 92 and 1000
//! were checked to produce identical outcomes on Windows and on Linux — which
//! is what makes a seed worth sending to a colleague.
//!
//! What does NOT replay, and is excluded from the comparison on purpose: the
//! envelope counters. They measure how much material each gossip carried, which
//! is the system's entropy rather than the schedule's.
//!
//! ## Replaying one
//!
//! `LAIT_SIM_SEED=1327 cargo test -p runtime --lib convergence_simulation`
//!
//! One seed, nothing else, about two seconds. The sweep prints that line on
//! failure, so a seed from nightly or from a colleague is a command rather than
//! an instruction to go and edit a test. Note the singular: `LAIT_SIM_SEEDS`
//! with an S is a COUNT, which is why the failure prints the command instead of
//! leaving anyone to work out which name takes what.
//!
//! ## Determinism is a claim this file has to earn
//!
//! [`Schedule`] is written out rather than taken as a dependency for the same
//! reason it is positional: a generator crate that seeds itself from the OS, or
//! a `HashMap` iterated anywhere, silently reintroduces the nondeterminism a
//! seed exists to remove — and the failure mode is a bug report nobody can
//! reproduce. Everything here is `BTreeMap`/`Vec`, and every choice comes from
//! `Schedule`.
//!
//! What is NOT simulated is time: no wall clock, no sleeps, no async. The
//! schedule is the order of operations, so there is nothing for a clock to
//! perturb at this seam. Time-dependent behaviour lives in the plane driver
//! above it and is simulated separately — `paused_clock` and `driver_beat` do
//! that with tokio's virtual clock. Driving both a fault schedule and a clock
//! together is the piece nobody has built.
//!
//! ## What it asserts
//!
//! Convergence alone is a weak claim: four empty replicas agree perfectly. So
//! the simulation also tracks what it asked for. Every commit adds a known
//! delta to a known counter, and the converged state must equal the sum of the
//! deltas that were actually committed. Agreement AND completeness — a dropped
//! transaction that never gets re-offered fails the second assertion even
//! though every peer is happily identical.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use mechanics::authorization::{
    AuthorizationDemand, AuthorizedBodyKey, PolicyCapability, Resource,
};
use mechanics::ids::SpaceId;
use replica::body::{
    BodyBinding, BodyId, BodyKey, EncodingId, Op, SchemaId, StaticBodyKeys, SupportedSchemas,
    WorldId, MUTATION_COLLABORATIVE,
};
use replica::frontier::AuthorityFrontier;
use replica::transaction::{
    AuthoritySource, CommitAuthorization, CommitContext, SeedSigner, StaticAuthorizer, Transaction,
    NO_PARENT_ROOT,
};
use replica::Replica;

const PEERS: usize = 4;
const BODIES: usize = 3;
const EPOCH: [u8; 16] = [5u8; 16];
const EPOCH_KEY: [u8; 32] = [6u8; 32];

/// A decision drawn from WHERE it is asked, not from how many were asked
/// before it.
///
/// This is the difference between a schedule that replays and one that does
/// not, and it took measuring to find. A sequential generator makes every
/// decision depend on the count of preceding draws — and that count is not a
/// function of the seed here, because `gossip` draws once per exported item and
/// `Replica::export_material` yields a varying number of items per run. It
/// varies because it groups material by transaction-commitment hash, and those
/// commitments embed entropy the harness cannot reach: sealing nonces
/// (`replica::content`), minted ids (`replica::ids`), FROST's own `OsRng`.
/// Seeding Fabric's identity minting removes one of those and not the rest.
///
/// Keying each decision by `(seed, step, purpose, index)` makes it independent
/// of every other decision. The system underneath may produce a different
/// number of envelopes on two runs; which STEP drops which envelope SLOT is the
/// same either way.
#[derive(Debug, Clone, Copy)]
struct Schedule {
    seed: u64,
}

/// What a draw is for. Distinct domains so two decisions at the same step and
/// index cannot collide into the same value.
mod purpose {
    pub const ACTION: u8 = 1;
    pub const PEER: u8 = 2;
    pub const BODY: u8 = 3;
    pub const DELTA: u8 = 4;
    pub const DROP: u8 = 5;
    pub const DUPLICATE: u8 = 6;
    pub const DELIVER: u8 = 7;
}

impl Schedule {
    fn new(seed: u64) -> Self {
        Self { seed }
    }

    fn draw(self, step: u32, purpose: u8, index: u32) -> u64 {
        let mut hash = blake3::Hasher::new();
        hash.update(b"lait.simulation.schedule.v1\0");
        hash.update(&self.seed.to_le_bytes());
        hash.update(&step.to_le_bytes());
        hash.update(&[purpose]);
        hash.update(&index.to_le_bytes());
        let digest = hash.finalize();
        let mut eight = [0u8; 8];
        eight.copy_from_slice(&digest.as_bytes()[..8]);
        u64::from_le_bytes(eight)
    }

    fn below(self, step: u32, purpose: u8, index: u32, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let bound = u64::try_from(n).expect("bound fits u64");
        usize::try_from(self.draw(step, purpose, index) % bound).expect("below n fits usize")
    }

    fn chance(self, step: u32, purpose: u8, index: u32, percent: u64) -> bool {
        percent > 0 && self.draw(step, purpose, index) % 100 < percent
    }
}

fn space() -> SpaceId {
    SpaceId::from_digest([16u8; 16])
}

fn authority_frontier() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![7])
}

fn world() -> WorldId {
    WorldId::parse("com.example.notes").expect("static world id")
}

fn body(index: usize) -> BodyKey {
    let tag = u8::try_from(index).expect("BODIES is small");
    BodyKey::new(world(), BodyId::from_bytes([tag; 16]))
}

/// One device seed per peer, so every transaction carries a real and distinct
/// signature rather than the fleet sharing one identity.
fn seed_of(peer: usize) -> [u8; 32] {
    let tag = u8::try_from(peer).expect("PEERS is small");
    [0x40 | tag; 32]
}

/// Authorizes exactly the fleet's devices. Not a blanket yes: material signed
/// by a device outside the fleet must still be refused, and a simulation that
/// accepted anything would stop testing the legitimacy check it drives.
struct Fleet {
    devices: BTreeSet<[u8; 32]>,
}

impl Fleet {
    fn new() -> Self {
        Self {
            devices: (0..PEERS)
                .map(|p| {
                    mechanics::actor::device_from_seed(&seed_of(p))
                        .key_bytes()
                        .expect("device key")
                })
                .collect(),
        }
    }
}

impl AuthoritySource for Fleet {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        self.devices.contains(signer)
    }
}

fn supported() -> SupportedSchemas {
    let mut s = SupportedSchemas::new();
    s.declare(
        world(),
        SchemaId::parse("note").expect("schema id"),
        1,
        EncodingId::parse("collab").expect("encoding id"),
        MUTATION_COLLABORATIVE,
    );
    s
}

fn binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("note").expect("schema id"),
        schema_version: 1,
        encoding: EncodingId::parse("collab").expect("encoding id"),
        mutation_model: MUTATION_COLLABORATIVE,
    }
}

fn replica() -> Replica {
    let mut r = Replica::loro().with_keys(Arc::new(StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
    )));
    r.set_supported(supported());
    r
}

/// One transaction in flight toward one peer. Held whole rather than as frames
/// because what this simulation perturbs is DELIVERY — order, loss,
/// duplication — and the framing layer is `freight_wire`'s subject.
#[derive(Clone)]
struct Envelope {
    to: usize,
    tx: Transaction,
    payloads: Vec<(BodyKey, Vec<u8>)>,
    /// Where this envelope sits in the schedule, not what it contains.
    ///
    /// Delivery is decided per envelope from `(step, label)` rather than by
    /// picking an index out of the queue, because queue LENGTH varies between
    /// runs even when the schedule does not. A label makes the decision a
    /// function of position: slot 3 of peer 1's gossip to peer 2 either
    /// arrives at this step or does not, whatever happens to be in it.
    label: u32,
}

struct Sim {
    schedule: Schedule,
    step_index: u32,
    peers: Vec<Replica>,
    inflight: Vec<Envelope>,
    partitioned: Vec<bool>,
    /// What the simulation asked for: per body, the sum of every delta that a
    /// commit actually accepted. The converged fleet must match this.
    expected: BTreeMap<usize, i64>,
    request_counter: u64,
    dropped: usize,
    duplicated: usize,
    delivered: usize,
    commits: usize,
}

impl Sim {
    fn new(seed: u64) -> Self {
        Self {
            schedule: Schedule::new(seed),
            step_index: 0,
            peers: (0..PEERS).map(|_| replica()).collect(),
            inflight: Vec::new(),
            partitioned: vec![false; PEERS],
            expected: (0..BODIES).map(|b| (b, 0)).collect(),
            request_counter: 0,
            dropped: 0,
            duplicated: 0,
            delivered: 0,
            commits: 0,
        }
    }

    /// Peer `p` commits `delta` to body `b`. Recorded in `expected` only when
    /// the commit succeeds, so a refused commit cannot make the completeness
    /// assertion demand state that was never authored.
    fn commit(&mut self, p: usize, b: usize, delta: i64) {
        let space = space();
        let seed = seed_of(p);
        let signer = SeedSigner(&seed);
        let ctx = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: authority_frontier(),
        };
        let authorizer = StaticAuthorizer {
            world: world(),
            implementation_id: [0u8; 32],
        };
        let demand = AuthorizationDemand::require(
            PolicyCapability::new(world().as_str(), "write"),
            Resource::root(world().as_str()),
        )
        .encode_canonical()
        .expect("demand encodes");

        self.request_counter += 1;
        let mut request = [0u8; 16];
        request[..8].copy_from_slice(&self.request_counter.to_le_bytes());

        let outcome = self.peers[p].commit_action(
            &ctx,
            &CommitAuthorization {
                actor: "actor",
                parent_manifest_root: NO_PARENT_ROOT,
                demand,
                intent_digest: [1u8; 32],
                authorizer: &authorizer,
            },
            &world(),
            &mechanics::actor::device_from_seed(&seed),
            &request,
            &[1u8; 32],
            vec![],
            vec![],
            "bump",
            &[
                (body(b), Op::Create),
                (
                    body(b),
                    Op::CounterAdd {
                        path: "votes".into(),
                        delta,
                    },
                ),
            ],
            &[(body(b), binding())],
            &[],
        );
        if outcome.is_ok() {
            *self.expected.entry(b).or_insert(0) += delta;
            self.commits += 1;
        }
    }

    /// Peer `p` offers everything it holds to every other peer, minus what the
    /// destination has already declared — the O(changed) export path, driven
    /// here rather than described.
    fn gossip(&mut self, p: usize) {
        for to in 0..PEERS {
            if to == p {
                continue;
            }
            let held = self.held_by(to);
            let Ok(material) = self.peers[p].export_material_excluding(&held) else {
                continue;
            };
            for (slot, (tx, payloads)) in material.into_iter().enumerate() {
                // The index mixes the destination and the slot so peer 1's
                // third item to peer 2 is a different decision from its third
                // item to peer 3.
                let index = u32::try_from(to * 64 + slot).unwrap_or(u32::MAX);
                // Loss happens at enqueue: the sender believes it sent.
                if self
                    .schedule
                    .chance(self.step_index, purpose::DROP, index, 12)
                {
                    self.dropped += 1;
                    continue;
                }
                let envelope = Envelope {
                    to,
                    tx,
                    payloads,
                    label: index,
                };
                // Duplication: the same material offered twice must be
                // absorbed without changing anything the second time.
                if self
                    .schedule
                    .chance(self.step_index, purpose::DUPLICATE, index, 8)
                {
                    self.duplicated += 1;
                    self.inflight.push(envelope.clone());
                }
                self.inflight.push(envelope);
            }
        }
    }

    /// What `to` already holds, in the shape `export_material_excluding` wants.
    fn held_by(&self, to: usize) -> BTreeSet<(BodyKey, [u8; 32])> {
        self.peers[to].head_commitments().into_iter().collect()
    }

    /// Deliver the in-flight envelopes this step selects.
    ///
    /// Selection is per envelope, from its label, rather than an index into the
    /// queue: queue length varies between runs even when the schedule does not,
    /// so an index would make every later choice depend on how much material
    /// the system happened to produce. Reordering survives — an envelope
    /// enqueued first has no claim on arriving first — it just no longer
    /// depends on the queue's size.
    fn deliver(&mut self, aggression: usize) {
        let threshold = (aggression as u64 * 25).min(100);
        let mut delivering = Vec::new();
        let mut keeping = Vec::new();
        for envelope in std::mem::take(&mut self.inflight) {
            if self
                .schedule
                .chance(self.step_index, purpose::DELIVER, envelope.label, threshold)
            {
                delivering.push(envelope);
            } else {
                keeping.push(envelope);
            }
        }
        self.inflight = keeping;
        for envelope in delivering {
            if self.partitioned[envelope.to] {
                // A partitioned peer does not receive. The envelope is lost,
                // not queued — recovery has to come from a later offer, which
                // is what makes the heal phase meaningful.
                self.dropped += 1;
                continue;
            }
            self.incorporate(&envelope);
        }
    }

    fn incorporate(&mut self, envelope: &Envelope) {
        let space = space();
        // The signer in the context is the LOCAL peer's; legitimacy of the
        // material comes from the transaction's own signature checked against
        // `Fleet`, not from whoever is incorporating it.
        let seed = seed_of(envelope.to);
        let signer = SeedSigner(&seed);
        let ctx = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: authority_frontier(),
        };
        self.delivered += 1;
        let _ = self.peers[envelope.to].incorporate(
            &ctx,
            &envelope.tx,
            &envelope.payloads,
            &Fleet::new(),
        );
    }

    /// Heal the network and exchange until nothing new moves. This is the
    /// point the assertions are about: convergence is a claim about what
    /// happens once delivery is possible again, not about every intermediate
    /// state.
    fn heal(&mut self) {
        self.partitioned = vec![false; PEERS];
        // Bounded rather than `loop`: a fixpoint that needs more rounds than
        // this is a liveness bug, and hanging is a worse way to report it than
        // failing the assertions below.
        for _ in 0..(PEERS * 4) {
            self.inflight.clear();
            for p in 0..PEERS {
                self.gossip_reliably(p);
            }
            if self.inflight.is_empty() {
                break;
            }
            // Unconditional: healing means the network works again, not that
            // it works with probability p.
            for envelope in std::mem::take(&mut self.inflight) {
                self.incorporate(&envelope);
            }
        }
    }

    /// `gossip` without the fault injection — the healed network.
    fn gossip_reliably(&mut self, p: usize) {
        for to in 0..PEERS {
            if to == p {
                continue;
            }
            let held = self.held_by(to);
            let Ok(material) = self.peers[p].export_material_excluding(&held) else {
                continue;
            };
            for (slot, (tx, payloads)) in material.into_iter().enumerate() {
                self.inflight.push(Envelope {
                    to,
                    tx,
                    payloads,
                    label: u32::try_from(to * 64 + slot).unwrap_or(u32::MAX),
                });
            }
        }
    }

    fn step(&mut self) {
        let at = self.step_index;
        let schedule = self.schedule;
        match schedule.below(at, purpose::ACTION, 0, 100) {
            0..=44 => {
                let p = schedule.below(at, purpose::PEER, 0, PEERS);
                let b = schedule.below(at, purpose::BODY, 0, BODIES);
                let delta =
                    i64::try_from(schedule.below(at, purpose::DELTA, 0, 7)).expect("fits i64") - 3;
                self.commit(p, b, delta);
                self.gossip(p);
            }
            45..=84 => {
                let aggression = 1 + schedule.below(at, purpose::DELIVER, u32::MAX, 4);
                self.deliver(aggression);
            }
            85..=94 => {
                let p = schedule.below(at, purpose::PEER, 1, PEERS);
                self.partitioned[p] = !self.partitioned[p];
            }
            _ => {
                let p = schedule.below(at, purpose::PEER, 2, PEERS);
                self.gossip(p);
            }
        }
        self.step_index = self.step_index.saturating_add(1);
    }

    /// The converged counter for body `b` as peer `p` sees it, or None if that
    /// peer has no such Body yet.
    fn counter(&self, p: usize, b: usize) -> Option<i64> {
        self.peers[p]
            .read_collaborative(&body(b))
            .ok()
            .and_then(|view| view.counters.get("votes").copied())
    }
}

/// One simulation: `steps` scheduled operations, then heal, then assert.
///
/// Returns a description on failure rather than panicking so the caller can
/// print the seed with it — and that seed reproduces the run, on any machine.
/// A failure nobody can replay is most of the value of this technique thrown
/// away.
fn run(seed: u64, steps: usize) -> Result<Sim, String> {
    let mut sim = Sim::new(seed);
    for _ in 0..steps {
        sim.step();
    }
    sim.heal();

    for b in 0..BODIES {
        let expected = sim.expected.get(&b).copied().unwrap_or(0);
        // Agreement.
        let first = sim.counter(0, b);
        for p in 1..PEERS {
            let observed = sim.counter(p, b);
            if observed != first {
                return Err(format!(
                    "body {b}: peer 0 sees {first:?}, peer {p} sees {observed:?} — the fleet did not converge"
                ));
            }
        }
        // Completeness. Without this, four replicas that all lost the same
        // transaction agree perfectly and the simulation reports success.
        let observed = first.unwrap_or(0);
        if observed != expected {
            return Err(format!(
                "body {b}: converged on {observed}, but {expected} was committed — material was lost, not merely delayed"
            ));
        }
    }
    Ok(sim)
}

/// A fixed seed, so this file has one case whose behaviour is stable across
/// runs and whose statistics can be sanity-checked. `TigerBeetle`'s rule, and it
/// earns its keep: it is what tells you the generator still produces work
/// after a refactor, rather than quietly scheduling nothing.
#[test]
fn the_fleet_converges_on_a_known_seed() {
    let sim = run(92, 400).unwrap_or_else(|failure| panic!("seed 92: {failure}"));
    assert!(
        sim.commits >= 50,
        "seed 92 scheduled only {} commits — the generator has gone quiet",
        sim.commits
    );
    assert!(
        sim.dropped > 0 && sim.duplicated > 0,
        "seed 92 injected no faults ({} dropped, {} duplicated) — this is not testing what it claims",
        sim.dropped,
        sim.duplicated
    );
}

/// Many seeds, shallow. Distinct schedules are what buys coverage here, so
/// breadth beats depth per unit of time on the per-push tier.
///
/// Two knobs, a letter apart, and it matters which:
///
/// - `LAIT_SIM_SEED` (singular) runs **exactly one** schedule. This is the
///   replay path — a failing seed from nightly or from a colleague is a
///   command, not an instruction to go and edit a test.
/// - `LAIT_SIM_SEEDS` (plural) is a **count**, raising how many consecutive
///   seeds the sweep explores.
///
/// Because those names are one character apart, the failure below prints the
/// command instead of leaving anyone to work out which takes what.
#[test]
fn the_fleet_converges_across_many_schedules() {
    if let Some(seed) = std::env::var("LAIT_SIM_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        if let Err(failure) = run(seed, 150) {
            panic!("seed {seed}: {failure}");
        }
        return;
    }

    let count: u64 = std::env::var("LAIT_SIM_SEEDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24);
    for seed in 1000..(1000 + count) {
        if let Err(failure) = run(seed, 150) {
            panic!(
                "seed {seed}: {failure}\n\n\
                 replay this exact run:\n  \
                 LAIT_SIM_SEED={seed} cargo test -p runtime --lib convergence_simulation\n\
                 it reproduces on any machine — that is what the seed is for."
            );
        }
    }
}

/// Material signed by a device outside the fleet is refused even when the
/// transfer itself is well formed. Asserted here rather than left implicit
/// because every other test in this file depends on `Fleet` being a real
/// check — if it authorized anything, the simulation would still pass and
/// would have stopped testing legitimacy.
#[test]
fn the_fleet_refuses_a_stranger() {
    let fleet = Fleet::new();
    let stranger = mechanics::actor::device_from_seed(&[0xEE; 32])
        .key_bytes()
        .expect("device key");
    assert!(!fleet.signer_authorized(&stranger, &authority_frontier()));
    for p in 0..PEERS {
        let member = mechanics::actor::device_from_seed(&seed_of(p))
            .key_bytes()
            .expect("device key");
        assert!(fleet.signer_authorized(&member, &authority_frontier()));
    }
}

/// What a run decided and what it reached — the part that must replay.
///
/// Deliberately excludes the envelope counters (`dropped`, `duplicated`,
/// `delivered`). Those measure how MUCH material each gossip carried, and that
/// varies between runs because `Replica::export_material` groups by
/// transaction-commitment hash and those commitments embed entropy no harness
/// can reach — sealing nonces, minted content ids, FROST's `OsRng`. They are
/// diagnostics, not the schedule.
fn outcome(seed: u64, steps: usize) -> String {
    let mut sim = Sim::new(seed);
    for _ in 0..steps {
        sim.step();
    }
    sim.heal();
    format!(
        "commits={} expected={:?} counters={:?}",
        sim.commits,
        sim.expected.values().collect::<Vec<_>>(),
        (0..BODIES).map(|b| sim.counter(0, b)).collect::<Vec<_>>()
    )
}

/// **The meta test.** A seed replays.
///
/// This is the property the whole technique rests on, and it did not hold at
/// first. The schedule used to come from a running generator, which coupled
/// every later decision to how many draws came before — and that count is not a
/// function of the seed, because `gossip` draws once per exported item and the
/// system yields a different number of items each run. Two runs stayed
/// identical for sixteen steps and then parted.
///
/// Decisions are now drawn from `(seed, step, purpose, index)`, so each one is
/// independent of every other. The system underneath may still produce
/// different material; which step commits what, and which envelope slot is
/// dropped, is the same either way.
///
/// Verified across machines as well as across runs: seeds 92 and 1000 produce
/// identical outcomes on Windows and on Linux. That is what makes a seed worth
/// sending to someone else.
#[test]
fn a_seed_replays_its_run() {
    for seed in [92u64, 1000, 4242] {
        let first = outcome(seed, 120);
        let second = outcome(seed, 120);
        assert_eq!(
            first, second,
            "seed {seed} produced two different runs — a seed that does not              replay cannot be handed to anyone"
        );
    }
}

/// And different seeds are different runs, or the seed is being ignored rather
/// than honoured and the test above could not tell the difference.
#[test]
fn different_seeds_are_different_runs() {
    let a = outcome(92, 120);
    let b = outcome(1000, 120);
    assert_ne!(
        a, b,
        "two seeds produced identical runs — the seed is inert"
    );
}
