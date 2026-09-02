//! Does the engine reach the browser target? This package is the measured
//! answer, not a claim: each `probe-*` feature pulls one engine crate into a
//! `wasm32-unknown-unknown` build, and `tests/smoke.rs` runs the CRDT core
//! inside a real JS host. See the manifest header for the commands, and
//! `ci/wasm-probe.sh` for all of them in order.
//!
//! Compiling is the floor, not the port. The journal and Replica cache still
//! speak `std::fs`, which on this target fails at runtime rather than at
//! build — honestly, per their `sync_dir` notes — so a browser store needs an
//! OPFS-shaped backend behind the same seams before any of this holds data.
//! What runs today, proven by the smoke tests: identity minting over JS
//! entropy, and fabric's fork → concurrent edit → exchange → converge cycle.

#[cfg(all(target_arch = "wasm32", feature = "probe-runner"))]
pub mod runner;

#[cfg(all(
    target_arch = "wasm32",
    feature = "probe-contact",
    feature = "probe-journal"
))]
pub mod space_pull;
