//! The canonical IssuesWorld product package.
//!
//! This crate owns the issue tracker's semantic World implementation, schemas,
//! product DTOs, and product identifiers. It depends inward on LAIT's generic
//! substrate and has no CLI, MCP, viewer, daemon, local-control, filesystem, or
//! process-lifecycle dependency.

pub mod contract;
pub mod dto;
pub mod ids;
mod implementation;
pub mod roles;
pub mod views;
pub mod workflow;

pub use contract::{IssueEffect, IssueIntent, IssueQuery, PRODUCT_WORLD};
pub use implementation::IssuesWorld;
