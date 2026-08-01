//! Every integration test in this package, as one binary.
//!
//! Cargo compiles each loose `tests/*.rs` into its own executable, and each one
//! statically links the whole dependency graph — iroh, loro, frost, rustls. At
//! 5 files that was 5 links for one `cargo test`, and on a Windows
//! runner linking is most of the wall clock.
//!
//! A directory with a `main.rs` is a SINGLE target, so these are modules now.
//! Test isolation is unchanged: nextest runs every test in its own process
//! regardless of which binary it came from.
//!
//! Add a file here and declare it below; nothing else changes.

mod flows;
mod identity_interop;
mod policy;
mod transport;
mod transport_capabilities;
