//! Every integration test in this package, as one binary.
//!
//! Cargo compiles each loose `tests/*.rs` into its own executable, and each one
//! statically links the whole dependency graph — iroh, loro, frost, rustls. At
//! 7 files that was 7 links for one `cargo test`, and on a Windows
//! runner linking is most of the wall clock.
//!
//! A directory with a `main.rs` is a SINGLE target, so these are modules now.
//! Test isolation is unchanged: nextest runs every test in its own process
//! regardless of which binary it came from.
//!
//! Add a file here and declare it below; nothing else changes.

mod adversary_incorporation;
mod algebra_fixtures;
mod batch_atomicity;
mod canonical_ids;
mod concurrent_heads;
mod content_declaration_convergence;
mod quotas_and_bundle;
mod store_growth;
mod transaction_marker_fixtures;
