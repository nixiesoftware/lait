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

//! The canonical IssuesWorld product package.
//!
//! This crate owns the issue tracker's semantic World implementation, schemas,
//! product DTOs, and product identifiers. It depends inward on LAIT's generic
//! substrate and has no CLI, MCP, viewer, daemon, local-control, filesystem, or
//! process-lifecycle dependency.

pub mod contract;
pub mod dto;
pub mod geometry;
pub mod ids;
mod implementation;
mod rank;
pub mod roles;
pub mod spec;
pub mod views;
pub mod workflow;

pub use contract::{IssueEffect, IssueIntent, IssueQuery, PRODUCT_WORLD};
pub use implementation::IssuesWorld;
