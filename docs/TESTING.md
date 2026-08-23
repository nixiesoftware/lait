# Testing lait

lait is a local-first, peer-to-peer, end-to-end-encrypted issue tracker: CRDTs
over a real network, threshold cryptography, a crash-safe journal, and a daemon
that has to behave the same on a named pipe as on a Unix socket. Almost every
interesting bug in that list is an *interleaving* — an operation order, a
delivery order, an interrupted write — and interleavings are not something you
enumerate by hand.

So the suite is organised by **what generates the cases**, and each tier runs
where its cost is affordable.

## The tiers

| tier | what generates cases | scope | where it runs | budget |
|---|---|---|---|---|
| **T0 laws** | proptest over random op programs | `fabric` | every push, all platforms | <1 s |
| **T1 contracts** | committed golden files, JSON Schema | DTO / wire / manifest | every push | seconds |
| **T2 behaviour** | hand-written examples | workspace | every push, **Linux**; OS seam on Windows/macOS | ~1 min |
| **T3 simulation** | a seed → a whole schedule | multi-peer convergence | every push (24 seeds), nightly (512) | ~6 s |
| **T4 reality** | real relays, full corpora | end to end | nightly / weekly | unbounded |

What separates the tiers is not importance. It is whether the cost is paid per
push by someone waiting. A 10,000-iteration FROST gate is not less important
than a DTO fixture; it is a thing to learn about within a day rather than
within a minute.

### T0 — the laws

`crates/fabric/src/convergence_laws_tests.rs`.

The collaborative algebra owes three laws, and they are asserted over randomly
generated programs rather than hand-picked ones: **convergence** (replicas that
saw the same operations project the same view), **commutativity and
associativity** (delivery order is not privileged), and **idempotence**
(re-delivering known material changes neither the view nor the `changed`
bookkeeping that decides whether a `Receipt` exists).

Ops that reference minted ids — `ListRemove`, `ListMove` — or in-bounds
coordinates — `TextSplice` — cannot be generated in isolation, so the generator
emits an *abstract* program that names its target positionally and lowers it
against the replica's own view at apply time.

Alphabets are deliberately tiny. A large value space would mean concurrent
replicas almost never touch the same path, and a convergence test where nothing
collides proves nothing.

```sh
cargo test -p fabric --lib convergence_laws        # 64 cases
PROPTEST_CASES=4096 cargo test -p fabric --lib convergence_laws
```

A failure writes its shrunk seed to `crates/fabric/proptest-regressions/`.
**Commit that file** — the counterexample then replays on every later run,
including the per-push tier.

The other generated suite at this tier is the parser:
`crates/runtime/src/internal_tests/contact_frame_fuzz.rs`. `ContactFrame`,
`Offer` and `Proof` all decode bytes from a peer *before* any signature is
checked — they have to, because the signature is inside the thing being
decoded — which makes them the outermost attack surface here, reachable by
anyone who can open a connection. It asserts that no input panics, that every
frame variant round-trips, that a frame must decode to exactly the bytes it
came from, and that a single flipped byte is survivable.

`presence_wire_fuzz.rs` does the same for the Beacon and presence decoders,
which are more exposed still: a Contact frame arrives on a connection someone
chose to open, while a Beacon arrives from anyone gossiping on the topic.

This is proptest rather than `cargo-fuzz` on purpose: `cargo-fuzz` needs
nightly and its own CI job, and a structure-aware generator on stable buys most
of the coverage for none of that. What it gives up is coverage-guided
exploration. If these files start finding things they cannot explain, that is
when to make the case for the nightly job.

### T1 — the contracts

Golden files and fixtures that pin the wire format, plus `ci/validate-dto-schema.py`,
which replays the committed schema examples through a non-Rust JSON Schema
2020-12 validator so the external contract is proven language-neutral rather
than merely serde-consistent. The TypeScript half of the naming rule runs in
the `viewer` job.

### T2 — the behaviour

The bulk of the suite: ~1,300 example-based tests. They say what the system
*means*, and they are worth keeping exactly as they are.

On Linux they all run on every push. On Windows and macOS they do not, and the
reason is arithmetic: the old matrix spent 19 of a 20-minute workflow on
`check (windows-latest)` — build 6.6 min, test 10.4 min — doing work the Linux
leg finished in 3. Windows is not slow because the tests are hard; it is slow
because a hosted two-core runner links eighty test binaries against iroh + loro
+ frost.

What those runners uniquely prove is that the **OS seam** holds, and most of
that proof is the compiler's. So the `platform` job type-checks every target
(`--all-targets`, which is what fails when a `UnixStream` or `std::os::fd`
reaches the control/lock path) and then runs only what no type check can see: a
named pipe that binds, an advisory lock that excludes a second instance, a path
that survives NTFS, a device name that is not a file, an fsync that lands.

That set is `[profile.pr-platform]`: **every crate's unit tests**, because
every `cfg(windows)`/`cfg(unix)` island in this workspace lives in a crate's
`src/`, plus the integration binaries that drive a real daemon over a real
control channel. Selecting by *kind* rather than by a list means it keeps
covering the seam when someone adds an island tomorrow.

### T3 — the simulation

`crates/runtime/src/internal_tests/convergence_simulation.rs`.

Four peers author concurrently while the network drops, duplicates, reorders
and partitions; then it heals and the fleet must agree.

**The seed replays the run**, verified across runs and across Windows and
Linux — the same seed produces the same commits, the same committed totals and
the same converged counters on both.

That did not hold at first, and finding out took measuring. The schedule came
from a running generator, which couples every later decision to how many draws
preceded it — and that count is not a function of the seed, because `gossip`
draws once per exported item and `Replica::export_material` yields a varying
number of items per run. It varies because it groups by transaction-commitment
hash, and those commitments embed OS entropy: sealing nonces, minted content
ids, FROST's own `OsRng`. Two runs stayed identical for sixteen steps and then
parted.

Seeding Fabric's identity minting was tried and does not fix it — there is no
single door at this layer, which is why madsim intercepts `getrandom`
process-wide instead. That seam was built, measured, and removed rather than
left in a security-sensitive path earning nothing.

The fix is in the harness: decisions are drawn from `(seed, step, purpose,
index)` rather than from a running generator, so each is independent of every
other. The system underneath may still produce different material on two runs;
which step commits what, and which envelope slot drops, is the same either way.

What does **not** replay, and is excluded from the comparison deliberately: the
envelope counters. They measure how much material a gossip carried, which is the
system's entropy rather than the schedule's.

```sh
cargo test -p runtime --lib convergence_simulation           # 24 seeds, ~12 s
LAIT_SIM_SEEDS=512 cargo test -p runtime --lib ...           # a wider sweep
LAIT_SIM_SEED=1327 cargo test -p runtime --lib ...           # ONE seed, ~2 s
```

**Sharing a seed.** The sweep prints the replay command on failure:

```
seed 1327: body 0: peer 0 sees Some(9), peer 2 sees Some(0) — the fleet did not converge

replay this exact run:
  LAIT_SIM_SEED=1327 cargo test -p runtime --lib convergence_simulation
it reproduces on any machine — that is what the seed is for.
```

Paste that line to a colleague and they get your failure. Verified end to end
with an injected bug: identical message on Windows and Linux.

Note the singular. `LAIT_SIM_SEED` is one seed; `LAIT_SIM_SEEDS` is a *count*.
They are a letter apart, which is why the failure prints the command rather
than leaving anyone to work out which takes what.

**Pair a shared seed with a commit.** A seed indexes into a schedule *this
code* generates; change the simulation and the same number means something
else. TigerBeetle states the same rule — the reproduction unit is the seed and
the git hash together, never the seed alone.

**Then put it in the corpus.** `simulation-seeds.txt` holds every seed that has
ever failed, and `every_seed_that_once_failed_still_passes` replays all of them
on every run. Without it the loop is: nightly finds a seed, someone fixes the
bug, the seed is never run again, and a regression reintroduces it in silence.

Add the seed *before* fixing, so you watch it go red and then green — a corpus
entry that was never seen to fail is a line nobody trusts. Its failure reads
differently from a sweep's on purpose: a sweep failure is a discovery, a corpus
failure is a regression.

The sweep also prints each seed **before** running it, not only on failure. A
seed that appears only when an assertion fires is no help when the run hangs or
the harness is killed by a timeout — which is exactly when knowing the schedule
matters most.

### T4 — reality

Nightly (`nightly.yml`) and weekly (`perf.yml`):

- the full workspace suite on all three platforms;
- the two mechanics ceremony gates at 10,000 iterations (133 s and 143 s *each*
  on an idle 12-core box);
- `commit_cost_baseline` at 50k and 100k Bodies (`LAIT_COMMIT_BASELINE_FULL`);
- the real-network iroh suites — excluded from the PR path not for time but for
  dependence: they carry retries because they are nondeterministic by nature,
  and a relay outage should not red a PR that never touched networking. iroh
  itself draws the line here, netsim in CI and real relays sparingly;
- `issues_reference_perf` at full corpus, weekly, on one pinned runner, because
  an absolute wall-clock budget measured on three differently-sized machines
  measures the machines.

`nightly.yml` also runs on pushes to `main`, so a platform regression is
attributed to the commit that caused it rather than discovered by whoever
pushes next.

## One test binary per package

Cargo compiles every `.rs` directly under `tests/` into its **own executable**,
and each one statically links the whole dependency graph — iroh, loro, frost,
rustls. At 70 files that was 70 links for one `cargo test`.

A directory containing `main.rs` is a *single* target, so every package's
integration tests live in `tests/it/` and are declared as modules:

```
tests/
  it/
    main.rs              <- mod cli_safety; mod lait_daemon; ...
    cli_safety.rs
    lait_daemon.rs
  clean_break_allowlist.tsv   <- data files stay put
```

Measured on a 12-core Windows box:

| | before | after |
|---|---|---|
| test targets in the workspace | 81 | **18** |
| rebuild after a one-line change in `replica` | 111 s | **32 s** |
| live test executables on disk | 3.3 GB | **238 MB** |

**Test isolation is unchanged.** nextest runs every test in its own process
regardless of which binary it came from, so tests that manipulate `LAIT_HOME`,
take the single-instance lock, or spawn a daemon are as isolated as they were
when each file was its own executable. (This would *not* be true under plain
`cargo test`, which runs a binary's tests in threads — one more reason nextest
is the runner here.)

A former per-file binary is now a module prefix on the test name, which is what
selectors say:

```
binary(orbital_boundaries)                     # before
binary_id(lait::it) & test(orbital_boundaries::)   # after
```

A bare `binary(mechanics)` still names a crate's **lib** target — its unit
tests — and is unrelated to any of this.

## Running a tier

The nextest profiles in `.config/nextest.toml` **are** the tier definitions, so
CI is reproducible locally after building the executable subjects and assembling
the independently installed World fixtures used by integration tests:

```sh
cargo build --workspace --locked --all-targets --all-features
bash ci/stage-test-worlds.sh
cargo nextest run --workspace --profile pr                     # what CI runs on Linux
cargo nextest run --workspace --profile pr-platform $(bash ci/platform-seam-targets.sh)
cargo nextest run --workspace --profile nightly                # everything
cargo nextest run --workspace                                  # default: also everything
```

`--profile default` has no `default-filter`, so any job passing its own `-E`
gets exactly what it asked for. A default filter there would silently narrow
such a filter, which is the failure the coverage manifest exists to prevent.

## The clock seam

The runtime drivers take their clock from `tokio::time::Instant`, not
`std::time::Instant`. That one-word difference is the whole mechanism:

- **Without** the `test-util` feature, `tokio::time::Instant::now()` compiles to
  `std::time::Instant::now()` — the same call, no indirection.
- **With** it, `tokio::time::pause()` freezes every call site at once and
  `advance()` moves them together.

So a test can drive a 30-second maintenance beat, a probation window, or a
connection deadline in microseconds, deterministically:

```rust
#[tokio::test(start_paused = true)]
async fn a_deadline_expires_when_the_clock_says() {
    let deadline = Instant::now() + Duration::from_secs(15);
    tokio::time::advance(Duration::from_secs(15)).await;
    assert!(Instant::now() >= deadline);
}
```

`crates/runtime/src/internal_tests/paused_clock.rs` is the worked example.

Three things worth knowing:

- **`test-util` is already on**, workspace-wide, in release too — `n0-future`
  (an iroh dependency) enables it unconditionally. `runtime` declares it in
  dev-dependencies anyway, so the seam does not silently break if that changes;
  if it did, `tokio::time::pause` would stop existing and the tests would fail to
  compile, which is the right way for it to break.
- **It costs an atomic load.** `Instant::now()` reads a "has anyone paused?"
  flag before falling through to the real clock. Next to the network I/O these
  loops do, it does not register — and it was already true of the
  `tokio::time::Instant` call sites in `src/`.
- **Not everything is covered.** `crates/replica` has no tokio dependency and
  keeps `std::time::Instant`, taking `now` as a parameter instead;
  `crates/comms` has one timing measurement; the head's own deadlines in `src/`
  are not worth simulating. `SystemTime::now` is a separate problem — see the gaps
  below.

Code that takes `now` as a parameter — `Gate::check(now)`, `budget.admit(now)`,
`replica.retained_content(now)` — was already testable and did not need any of
this. The seam is for code that asks the clock itself.

## The network simulator

`comms::mem::MemNet` is an in-process transport — the whole network over
channels, no iroh, no sockets. `MemNet::new()` delivers everything;
`MemNet::seeded(seed, Faults)` loses and doubles deliveries from one seeded
generator, and `partition(a, b)` / `heal()` cut and restore a pair.

The default is untouched on purpose. A fault injector that changes what
`MemNet::new()` does silently rewrites what thirty existing tests mean.

Gossip is judged at the **receiver**, direct connections at the **sender**. A
broadcast goes to a bus and the sender cannot know who is listening; filtering
per subscriber is also the more faithful model, because a partition means each
side independently stops hearing the other rather than the message never being
spoken.

A dropped dial returns a live handle nobody holds the other end of, rather than
an error — that is what a lost SYN looks like to a dialer. Returning `Err`
would be easier code and the wrong model.

**The test that matters is the one about the harness.** Seeding is worth having
because a failure replays, and that is a claim about the simulator rather than
the system — the one most likely to be quietly false. One `HashMap` iteration,
one unseeded generator, and the seed determines nothing while every test still
passes. So `the_same_seed_replays_exactly` runs one seed twice and compares a
trace of every delivery decision line by line, not by count: two runs that drop
the same *number* of messages and different messages are not the same run. Its
companion asserts a different seed diverges, because otherwise the seed could be
ignored rather than honoured and the first test could not tell.

**Verified across machines**, not just across runs: seed `0xA11CE` produces a
byte-identical 28-line trace on Windows and on Linux. That is what makes a seed
shareable — a teammate given one reproduces the run. It holds here because
every fault decision is made inside `MemNet`, where nothing reaches the OS.
Contrast T3 above, where the system under test draws its own entropy.

**Delay is not modelled**, deliberately. Drop, duplicate and partition are
decided synchronously at the delivery point, which is what keeps the decision
order a function of the seed. Delay needs a timer per message and a task per
timer, and once deliveries race on the scheduler the seed stops determining the
order. It is buildable against a paused clock, but it is a second mechanism
rather than a knob on this one.

## Fuzzing

Two layers, same property, different search.

`contact_frame_fuzz.rs` and `presence_wire_fuzz.rs` generate structured inputs
with proptest and run on every push. `fuzz/` holds byte-slice targets that
`nightly.yml` runs under libFuzzer for ten minutes each — libFuzzer watches
which branches an input reached and mutates toward the ones it has not, finding
inputs a blind generator would need a very long time to stumble into.

| target | decoder |
|---|---|
| `contact_frame` | `ContactFrame::decode` |
| `handshake` | `Offer::decode`, `Proof::decode` |
| `beacon` | `SignedBeacon::decode_canonical` |
| `presence` | `PresenceProbe::decode`, `PresenceAck::decode` |

Byte-slice targets, so there are no generators to duplicate. That is also why
`cargo-fuzz` rather than `bolero`: bolero would unify the property test and the
fuzz target under one harness, but only by rewriting the generators in its own
API — and for a parser the coverage-guided value is in the raw-bytes path,
which needs no generator at all.

`fuzz/` is a package but **not** a workspace member. cargo-fuzz needs nightly
for its `-Z` sanitizer flags, and this workspace pins `channel = "stable"` with
an MSRV floor CI checks; excluding it means `cargo build`, `cargo test`,
`cargo clippy` and the MSRV job never see it. The nightly job overrides its own
toolchain with `RUSTUP_TOOLCHAIN`, the same mechanism `msrv (1.91)` uses to pin
downward.

```sh
cargo fuzz list                                    # the targets
RUSTUP_TOOLCHAIN=nightly cargo fuzz run beacon     # Linux; see the gap below
```

A crash artifact is the output worth having: it is real bytes, so the next step
is a regression test rather than a reproduction from a description.

Measured under WSL before this shipped, 45 seconds per target — around 200,000
executions a second, no crashes, and every target reporting new coverage units,
which is what distinguishes fuzzing from spinning:

| target | runs | new coverage units |
|---|---|---|
| `contact_frame` | 9,544,869 | 1288 |
| `handshake` | 7,774,420 | 301 |
| `beacon` | 9,994,113 | 754 |
| `presence` | 10,665,856 | 223 |

At that rate nightly's 600-second budget is roughly 120 million executions per
target. **Windows cannot run this** — see the gap list — so WSL is the local
route.

## Mutation testing

`nightly.yml`'s `mutants` job runs `cargo-mutants`: it breaks the code on
purpose — flips a comparison, returns a default, deletes a branch — and reports
which breakages the suite fails to notice.

It exists because the generated suites were each mutation-tested **by hand**
when written, and nothing kept them honest afterwards. That is how we know a
peer that discards material fails T3's agreement assertion, and that deleting
`decode_canonical`'s re-encode fails the canonicity case. Those checks were a
moment in time; this is the standing version.

`.cargo/mutants.toml` scopes it to four files:

| file | the claim its tests make |
|---|---|
| `crates/fabric/src/fabric.rs` | convergence, commutativity, idempotence over random programs |
| `crates/runtime/src/plane/contact.rs` | the Contact decoder never panics and is canonical |
| `crates/runtime/src/beacon.rs` | same, for announcements from anyone on the topic |
| `crates/runtime/src/neighbor_presence.rs` | same, for the presence challenge |

459 mutants, ~4 s build + ~4 s test each, across 8 shards. Pointed at the whole
workspace it would take a week and mostly rediscover that example-based tests do
not cover every branch, which nobody disputes. A surviving mutant *here* is
interesting: it means a suite that claims to explore a space has a hole in it.

`[profile.mutants]` in `.config/nextest.toml` is the suite it runs — the
generated tests and the fixtures around them, without the ceremony gates whose
140 seconds each would be inside a per-mutant multiplication.

**It reports rather than gates.** Survivors are uploaded as an artifact to be
read and argued with. Making the score a build failure is a decision to take
once there is a baseline worth holding.

```sh
cargo mutants --workspace --list          # what would be mutated
cargo mutants --workspace --shard 0/8     # one shard, as CI runs it
```

## Whole-stack determinism

`runtime`'s convergence simulation replays its **schedule**. `sim/` replays the
**bytes** — same transaction commitments, same sealed payloads, same minted ids,
verified byte-identical across Windows and Linux.

That needs two doors shut, and `sim/.cargo/config.toml` shuts both at compile
time:

| flag | closes |
|---|---|
| `getrandom_backend="custom"` | every random byte in the whole dependency graph — lait's own calls, Replica's sealing nonces, Mechanics' key material, and FROST's `rand_core::OsRng`, which sits on getrandom |
| `lait_simulation` | Loro recording a wall-clock second into every change |

**Why below the code rather than inside it.** A seam was tried first, on
`fabric::op`'s identity minting, and removed again: it covered one entropy
source of four, and it put a runtime switch in the middle of security-critical
code. There is no single door *inside* the stack. There is one underneath, and
getrandom is it.

**Why it cannot reach production.** These are rustc cfgs in a package's own
config file. Cargo reads configuration from the current directory upward and
never downward, so a build at the repo root cannot see them, and `sim` is
excluded from the workspace besides. There is no runtime switch, no feature to
enable by accident.

**Why a seeded nonce is not a vulnerability here.** AEAD needs nonce
*uniqueness*, which the generator provides — it never repeats within a run.
What it gives up is unpredictability, which matters for material an adversary
sees; a simulation's temporary store outlives nothing.

**nextest, not `cargo test`.** The generator is one counter per process, so
tests sharing a process consume each other's stream. Process-per-test isolation
is also what makes a seed shareable at all: a colleague's fresh process starts
where yours did.

```sh
cd sim && cargo nextest run          # the whole stack, from a seed
```

One near-miss worth keeping in mind. With entropy seeded but the clock still
recorded, the determinism test *passed* — because both runs happened inside the
same second. `a_second_of_wall_clock_does_not_change_the_bytes` sleeps 1.5 s
between runs for exactly that reason. A reproducibility claim that holds only
within one second is not one.

## The coverage manifest

`ci/coverage-manifest.txt` records every test id in the workspace, what each
tier selects, and what each named release gate selects.

It replaced twelve `orbital-*` jobs that re-ran, per push, a slice of the suite
the workspace job had already run — ~23 runner-minutes to assert that some
tests *exist*. That goal was right and the mechanism was expensive and weaker
than it looked: it relied on nextest's exit-4 "filter matched nothing", so
deleting one test from a module that still had others left every gate green.
Recording every id catches that; a deletion is a reviewable line.

```sh
bash ci/coverage-manifest.sh --check     # regenerate and diff
bash ci/coverage-manifest.sh --update    # accept the change (Linux only)
```

**The manifest is a Linux artifact.** Four `#[cfg(unix)]` tests do not exist on
Windows, so regenerating there records their absence as coverage loss.
`--update` refuses to run off Linux, and the CI job uploads the corrected file
as an artifact so the fix is a download rather than a hunt for a Linux machine.

A diff here is a coverage change. It should be reviewed like one.

## Adding a test

- **A new law or invariant about the algebra** → T0, as a property.
- **A new wire shape** → T1, with a golden file, and re-run the manifest.
- **A new behaviour** → T2, next to its subject as a `#[cfg(test)]` module in
  the crate that owns it. If it genuinely needs the package's public surface
  from outside, add a file to that package's `tests/it/` and declare it in
  `tests/it/main.rs` — **not** a loose `tests/*.rs`, which cargo would compile
  into its own binary.
- **A new failure mode in delivery** → T3, as a fault the simulation can inject.
- **Anything that takes minutes, or touches a relay** → T4, and say so in
  `.config/nextest.toml` so the tier profiles keep it off the PR path.

Then `bash ci/coverage-manifest.sh --update` and commit the manifest with it.

## What this suite does not do

Written down because a gap you know about is a decision, and a gap you don't is
a surprise.

- **`MemNet` injects faults per connection, not per frame.** `connect`,
  `connect_session` and gossip delivery are gated; the byte stream inside an
  established session is not — measured, a whole fetch shows `sent=1`. What
  per-frame faults would buy is partial-transfer-under-loss, and
  `an_interrupted_transfer_resumes_and_installs_only_after_verification` already
  covers resume by removing the provider mid-fetch.
- **One `SystemTime::now` is deliberately unseamed**: `fabric`'s entropy
  fallback hashes the clock into a substitute RNG when the OS entropy source
  fails. That is not a timestamp read and freezing it would defeat the point.
- **Coverage-guided fuzzing does not run on Windows.** `cargo fuzz build` fails
  to link `irpc` and `iroh-relay` under MSVC, with and without sanitizers —
  both are transitive iroh dependencies built as DLLs, and cargo-fuzz's link
  arguments do not suit a DLL. It builds and runs on Linux; a Windows developer
  wanting to run it locally needs WSL.
- **Mutation testing reports, it does not gate.** `mutants` in `nightly.yml`
  breaks the code on purpose and lists what the suite fails to notice. Turning
  a score into a build failure is a decision to take once there is a baseline
  to hold; a gate that fails on day one is a job someone turns off.
- **`sim/`'s determinism does not extend to the drivers.** The whole-stack tier
  closes entropy and clock for an authoring workload; the driver-layer
  simulation in `freight_two_node.rs` runs against the real clock seam and a
  seeded network, but not under `sim/`'s cfgs. Running the two together is
  possible and has not been needed.
- **T3's envelope counts do not replay**, only its schedule and outcome. The
  counts depend on how much material `export_material` yields, which varies with
  entropy the workspace build does not close — `sim/` is where bit-exactness
  lives.
