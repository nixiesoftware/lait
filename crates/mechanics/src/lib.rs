#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! **The lait kernel** — lait's roots, in the sense of a seed, not an OS core:
//! the minimal set of commitments everything else is derived from, and against
//! which every scaffold is replaceable.
//!
//! This crate lists **no scaffold** in its manifest — no CRDT engine, no transport — so a
//! scaffold reference here does not compile. That absence *is* the boundary:
//! "where lait starts and ends" is the dependency edge, enforced by rustc.
//!
//! The kernel determines **legitimacy** — identity, authority, custody,
//! recovery, and which transitions are valid given signed history. `fabric`
//! maintains the **shared world** — documents, persistence, history,
//! convergence, projection. They are separate crates because that dependency
//! edge is a correctness boundary: convergence cannot confer legitimacy. They
//! ship, test, and version together as lait's substrate.
//!
//! What lives here is pure over identity + signed bytes:
//!
//! - [`ids`] — self-certifying identity types (a `DeviceId` *is* an ed25519 key).
//! - [`crypto`] — sealing/identity primitives (pure RustCrypto/dalek).
//! - [`sigdag`] — the signed hash-DAG envelope every trust plane rides.
//! - [`genesis`] — the root of trust that seeds every replay.
//! - [`acl`] / [`actor`] / [`space`] — the trust planes: membership authority,
//!   actor/device identity, and break-glass recovery, each a pure replay over
//!   signed bytes (a scaffold only *moves* those bytes; trust comes from replay).
//! - [`dkg`] — the FROST threshold-recovery ceremony logic.
//! - [`authz`] — authorization decisions over the replayed state.

mod acl;
pub mod actor;
pub mod assignment;
mod authority;
pub mod authorization;
mod ceremony;
mod compile;
mod crypto;
mod custody;
mod demand;

mod dkg;
mod expand;
#[cfg(test)]
mod gaccess;
#[cfg(test)]
mod gdkg;
mod genesis;
#[cfg(test)]
mod handover;
pub mod ids;
mod ledger;
pub mod membership;
pub mod policy;
pub mod recovery;
#[cfg(test)]
mod refresh;
#[cfg(test)]
mod reshare;
mod secretfs;
mod sigdag;
pub mod space;
pub mod station;
mod transition;

#[cfg(test)]
extern crate self as mechanics;
#[cfg(test)]
mod authority_checkpoint_tests;
#[cfg(test)]
mod frontier_isolation_tests;
#[cfg(test)]
mod sparse_ceremony_tests;
