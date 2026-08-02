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
and partitions; then it heals and the fleet must agree. The schedule comes from
a seed, so a failure reproduces exactly.

Two assertions, because convergence alone is weak — four empty replicas agree
perfectly. Every commit adds a known delta to a known counter, so the fleet must
agree **and** agree on the total that was actually committed. A transaction lost
and never re-offered fails the second even though every peer is identical.

The PRNG is written out rather than taken as a dependency. A simulation is only
replayable if it has exactly one source of entropy, and a crate that seeds
itself from the OS — or a `HashMap` iteration order — silently reintroduces the
nondeterminism the seed exists to remove.

**Time is not simulated here, and does not need to be.** T3 drives
`incorporate` and `export_material`; neither consults a clock, so there is
nothing at this seam for a simulated one to advance. The schedule *is* the
order of operations.

Time-dependent behaviour lives in two other places, and both are covered
separately: the plane driver, under a paused clock (see "The clock seam"), and
content-hold expiry, which takes its instant as a parameter. The version of
this that remains undone is a **driver-layer** simulation — a fault-injected
schedule and a simulated clock advancing together — and what it needs is
Station scaffolding, not more clock work.

```sh
cargo test -p runtime --lib convergence_simulation      # 24 seeds
LAIT_SIM_SEEDS=512 cargo test -p runtime --lib convergence_simulation
```

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
CI is reproducible locally with one command:

```sh
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
  `crates/comms` has one timing measurement; CLI deadlines in `src/` are not
  worth simulating. `SystemTime::now` is a separate problem — see the gaps
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

- **No whole-system deterministic simulation.** T3 is deterministic at the
  Replica/convergence seam, where there is no I/O. The clock is now controllable
  in `crates/runtime` (see "The clock seam"), which is the piece that was
  missing; what remains is entropy and network scheduling. Extending through
  `comms` for real means `madsim`-style libc interception — S2 found `turmoil`
  alone insufficient, because timestamps in packets, `HashMap` ordering, and
  dependencies making uncontrolled syscalls all leaked through.
- **One `SystemTime::now` is deliberately unseamed**: `fabric`'s entropy
  fallback hashes the clock into a substitute RNG when the OS entropy source
  fails. That is not a timestamp read and freezing it would defeat the point.
- **Coverage-guided fuzzing does not run on Windows.** Verified rather than
  assumed: `cargo fuzz build` fails to link `irpc` and `iroh-relay` under MSVC,
  with and without sanitizers — both are transitive iroh dependencies built as
  DLLs, and cargo-fuzz's link arguments do not suit a DLL. The targets
  type-check on stable Windows; the libFuzzer link is what cannot happen there.
  Nightly runs it on Linux.
- **Mutation testing reports, it does not gate.** `mutants` in `nightly.yml`
  breaks the code on purpose and lists what the suite fails to notice. Turning
  a score into a build failure is a decision to take once there is a baseline
  to hold; a gate that fails on day one is a job someone turns off.
- **Time is not simulated anywhere.** See T3 above; this is the largest
  remaining gap and the most expensive to close.
