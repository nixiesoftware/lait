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

mod action;
mod admission;
pub mod beacon;
mod budget;
mod contact_driver;
mod content_host;
pub mod coordinates;
#[cfg(test)]
mod dispatch_tests;
mod dto;
mod fetch;
pub mod generation;
mod implementation;
#[cfg(test)]
mod internal_tests;
mod lifecycle;
pub mod neighbor;
mod neighbor_presence;
mod neighbors;
pub mod plane;
mod plane_driver;
mod plane_stream;
pub mod poison;
mod registry;
mod session;
pub mod signal;
mod store;
mod transfer;
pub mod transient;
pub(crate) mod wire;
pub mod world;

#[cfg(test)]
extern crate self as runtime;

pub use lifecycle::Failure as Error;
pub use lifecycle::{
    Exit, ExitReason, Integrity, Interruption, Orbit, OrbitStatus, Persistence,
    RemovalConfirmation, Runtime, Station, StorageReading,
};
pub use session::{Session, WorldGeneration, WorldSnapshotId};
