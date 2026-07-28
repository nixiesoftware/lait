//! Composition-layer DTO re-exports from the bundled Issues package.
//!
//! Older root modules and generic Space projections continue to use
//! `lait::dto`; product protocols and presenters import the canonical
//! definitions directly from IssuesWorld.

pub use issues::dto::*;
