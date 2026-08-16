//! Every integration test in this package, as one binary.
//!
//! Cargo compiles each loose `tests/*.rs` into its own executable, and each one
//! statically links the whole dependency graph — iroh, loro, frost, rustls. At
//! 11 files that was 11 links for one `cargo test`, and on a Windows
//! runner linking is most of the wall clock.
//!
//! A directory with a `main.rs` is a SINGLE target, so these are modules now.
//! Test isolation is unchanged: nextest runs every test in its own process
//! regardless of which binary it came from.
//!
//! Add a file here and declare it below; nothing else changes.

mod adoption;
mod beacon_presence_fixtures;
mod contact_fixtures;
mod contact_iroh;
mod contact_mem;
mod coordinates_fixtures;
mod independent_world;
mod live_acceptance;
mod live_media;
mod plane_fixtures;
mod reciprocal_dial_loop;
mod signal_wire;
mod two_node_convergence;
