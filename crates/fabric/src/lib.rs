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

//! The Engine maintains the **shared world**: collaborative documents,
//! persistence, history, convergence, and projection.
//!
//! The kernel determines **legitimacy** — identity, authority, custody,
//! recovery, and which transitions are valid given signed history. The Engine
//! and the kernel are separate crates because the dependency edge is a
//! correctness boundary: convergence cannot confer legitimacy. They ship, test,
//! and version together as lait's substrate.
//!
//! This crate is the substrate's Loro boundary. It owns container layouts, CRDT
//! mutations, import/export, and the collaborative-document seam the replica
//! drives ([`fabric::Engine`]); kernel replay adjudicates signed authority
//! inputs. Raw document handles never cross the boundary — everything outside
//! sees [`fabric::Op`] transactions and typed exports.

#[cfg(test)]
mod algebra_reservation_tests;
mod fabric;
mod loro_ext;
mod op;

mod causal;
pub use causal::{
    Anchor, AnchorResolution, Artifact, ArtifactRef, CausalRelation, CheckpointPolicy,
    ImportStatus, Invalid, Material, OpHead, Version, CAUSAL_FORMAT_VERSION, MAX_HEADS,
};
pub use fabric::{
    commit, projection, BodyExport, CausalToken, CollaborativeView, Engine, Key, ListElement, Op,
    Receipt, Transaction,
};
