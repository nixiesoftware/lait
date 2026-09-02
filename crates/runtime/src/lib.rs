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

//! **Runtime** — LAIT's orbital lifecycle.
//!
//! ```text
//! Space
//!   +-- Orbit: one device's durable relationship to the Space
//!         +-- Replica: durable local materialization
//!         +-- Station: that Orbit activated for exclusive local operation
//!               +-- hosted World implementation
//!                     +-- docked Session
//! ```
//!
//! Runtime owns the domain lifecycle: forming/entering/observing/acquiring
//! Orbits, activating them into Stations, hosting Worlds, docking Sessions,
//! Contact policy, and Observation publication. It exposes **no** CRDT, iroh,
//! stream, file, key, ciphertext, mutex, or product request types — those live
//! below the boundary in [`fabric`], [`comms`], and [`mechanics`].
//!
//! An Orbit is the durable relationship and persists while vacant or occupied.
//! The Rust handles encode its exclusive operational lease:
//! [`Orbit::activate`] consumes the vacant Orbit handle and returns a
//! [`Station`]; [`Station::vacate`] consumes the active Station handle and
//! returns a vacant Orbit handle. Those ownership transfers are not an
//! ontological conversion between Orbit and Station.
//!
//! S0 establishes the sealed lifecycle contract surface and a **real, tested**
//! immutable World registry (duplicate registration is rejected). The lifecycle
//! transitions are wired in later stages (Orbit in S2, Station in S3,
//! World/Session/Contact in S5); their signatures here fix ownership and
//! consumption semantics.

// The guest carve. A World runner links this crate for its contract surface —
// `world`, `exec`, `find`, `publication` and what they stand on — and that
// surface must reach wasm32, where there is no process, no thread, no file
// lock and no clock of the Station's kind. The modules below are the Station's
// own machinery: drivers, transports, stores, the lifecycle machine. They stay
// native. Gating is by target, never by feature, so a native build carries
// exactly what it always carried and the carve cannot be mis-composed.
mod action;
#[cfg(not(target_arch = "wasm32"))]
mod admission;
#[cfg(not(target_arch = "wasm32"))]
pub mod beacon;
pub(crate) mod body_image;
/// The browser composition root: the same Station machinery, composed by the
/// embedding Worker instead of the lifecycle. wasm32-only by design — on
/// native, the lifecycle's custody chain is the only door to a store.
#[cfg(target_arch = "wasm32")]
pub mod browser;
mod budget;
pub mod change;
#[cfg(not(target_arch = "wasm32"))]
mod contact_driver;
#[cfg(not(target_arch = "wasm32"))]
mod content_cursor;
#[cfg(not(target_arch = "wasm32"))]
mod content_host;
pub mod coordinates;
pub(crate) mod corpus;
pub(crate) mod corpus_store;
/// The identity-scoped correspondence dial tone (`lait/correspondence/1`).
#[cfg(not(target_arch = "wasm32"))]
pub mod correspondence;
#[cfg(test)]
mod dispatch_tests;
mod dto;
pub mod exec;
#[cfg(not(target_arch = "wasm32"))]
mod fetch;
pub mod find;
pub(crate) mod find_evaluator;
#[cfg(not(target_arch = "wasm32"))]
pub mod generation;
mod implementation;
#[cfg(test)]
mod internal_tests;
#[cfg(not(target_arch = "wasm32"))]
mod lifecycle;
#[cfg(not(target_arch = "wasm32"))]
pub mod neighbor;
#[cfg(not(target_arch = "wasm32"))]
mod neighbor_presence;
#[cfg(not(target_arch = "wasm32"))]
mod neighbors;
#[cfg(not(target_arch = "wasm32"))]
mod peer_supply;
#[cfg(test)]
mod placement_tests;
pub mod plane;
#[cfg(not(target_arch = "wasm32"))]
mod plane_driver;
#[cfg(not(target_arch = "wasm32"))]
mod plane_stream;
pub mod poison;
pub mod publication;
mod registry;
mod session;
pub mod signal;
#[cfg(not(target_arch = "wasm32"))]
mod store;
#[cfg(not(target_arch = "wasm32"))]
mod transfer;
pub mod transient;
pub(crate) mod wire;
pub mod world;

#[cfg(test)]
extern crate self as runtime;

/// Exported without the cursor: outside this crate a read goes through
/// [`Station::content_acquire`], which owns the supply.
#[cfg(not(target_arch = "wasm32"))]
pub use content_cursor::Gap;
#[cfg(not(target_arch = "wasm32"))]
pub use lifecycle::Failure as Error;
#[cfg(not(target_arch = "wasm32"))]
pub use lifecycle::{
    Acquired, Exit, ExitReason, Integrity, Interruption, Orbit, OrbitStatus, Persistence,
    RemovalConfirmation, Runtime, Station, StorageReading,
};
pub use session::{
    AffectedWorldPublication, DurableOperationReceipt, LifecycleSourceStatus, Observation,
    ObservationStream, OperationPublication, OperationStatus, Session, WorldGeneration,
    WorldSnapshotId,
};
