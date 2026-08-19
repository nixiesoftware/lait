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

//! **Replica** — LAIT's durable-material and Convergence semantics.
//!
//! A Replica is an Orbit's durable local materialization of its Space: authority
//! material, World Bodies, semantic frontiers, locally held keys, and enough
//! metadata to distinguish unknown, partial, and corrupt material. Replica is a
//! LAIT semantic type — **not the CRDT engine**, which it never exposes. It applies
//! transaction, incorporation, and Convergence policy using [`mechanics`]
//! (mechanics) for legitimacy and [`Engine`] (Engine) for canonical
//! collaborative representation and durability.
//!
//! This crate is prefix-free from birth (the S8 renames do not touch it). It
//! names neither `loro` nor any product/consumer vocabulary — the dependency
//! edge is the seal, and the guard suite proves the vocabulary boundary.
//!
//! The sealed contract surface: Body identity and schemas ([`body`]),
//! operations/descriptors ([`body`]), semantic/authority frontiers
//! ([`frontier`]), Convergence outcomes ([`convergence`]), signed transactions
//! and manifests ([`transaction`], [`manifest`]), persistent-idempotency
//! receipts ([`receipt`]), and the committing [`replica`] itself, which
//! translates validated Body operations into Engine operations and advances
//! only from durable Engine receipts.

mod algebra;
pub mod body;
mod cache;
pub mod content;
pub mod convergence;
pub mod frontier;
mod ids;
mod index;
pub mod manifest;
mod protected;
pub mod receipt;
mod replica;
pub mod transaction;

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod canonical_store_tests;
#[cfg(test)]
mod content_fixture_tests;
#[cfg(test)]
mod content_plane_tests;
#[cfg(test)]
mod manifest_atomicity_tests;
#[cfg(test)]
mod manifest_fixture_tests;

pub use replica::{
    generation, BodyImageBounds, BodyImageFailure, BodyImageId, BodyImagePresence, BodyIx,
    GenerationFootprint, GenerationReader, GenerationSourceFootprint, ReadGeneration, ReadSnapshot,
    ReceiptAbsence, ReceiptCheck, ReceiptFootprint, ReceiptReader, Replica,
};
