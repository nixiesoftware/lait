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

**Time is not simulated.** The schedule is the order of operations, so there is
nothing for a clock to perturb at this seam. Time-dependent behaviour lives in
the plane driver above it, and simulating that means intercepting `Instant::now`
across the workspace — 180 call sites — which is a deliberate project, not a
side effect of this one. See "What this suite does not do" below.

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
  the crate that owns it. Integration binaries under `tests/` are the older
  layout; each one is a separate link against the whole dependency graph.
- **A new failure mode in delivery** → T3, as a fault the simulation can inject.
- **Anything that takes minutes, or touches a relay** → T4, and say so in
  `.config/nextest.toml` so the tier profiles keep it off the PR path.

Then `bash ci/coverage-manifest.sh --update` and commit the manifest with it.

## What this suite does not do

Written down because a gap you know about is a decision, and a gap you don't is
a surprise.

- **No whole-system deterministic simulation.** T3 is deterministic at the
  Replica/convergence seam, where there is no I/O. Extending it through `comms`
  means controlling time and entropy for the whole process — `madsim`-style
  libc interception, or `turmoil` plus the same. S2 found turmoil alone
  insufficient: timestamps in packets, `HashMap` ordering, and dependencies
  making uncontrolled syscalls all leaked through. This workspace has ~180
  `Instant::now`, 9 `SystemTime::now` and 31 `tokio::time::sleep` call sites
  that would need a seam first.
- **No fuzzing.** Loro fuzzes its own CRDT core; lait's encode/decode boundary
  (`postcard`, the Contact frames) has no `cargo-fuzz` target. The frame parser
  is the obvious first one.
- **No mutation testing in CI.** T0 and T3 were each mutation-tested by hand
  when written — that is how we know the assertions fail when the system
  breaks — but nothing keeps them honest automatically.
- **Test binaries are not consolidated.** ~70 files under `tests/` are ~70
  separate links against iroh + loro + frost, and `target/debug/deps` holds
  2.3 GB of test executables. Consolidating into one binary per directory is a
  known 2-4× on build time and has not been done.
