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

//! Identity-scoped address book: a typed projection over one Fabric Body.
//!
//! This crate is a leaf. It names `fabric` and `mechanics`. It does not name
//! daemon, Runtime, Replica, World, or a product. A0 ships no surface.

mod attest;
mod bounds;
mod bundle;
mod codec;
mod engine;
mod error;
mod ids;
mod mapping;
pub mod registry;
mod store;
mod types;

pub use attest::{names_of, parties_called, ResolvedName};
pub use bounds::{
    MAX_ADDRESSBOOK_HISTORY_BYTES, MAX_BOOK_BYTES, MAX_BUNDLE_BYTES, MAX_CARDS,
    MAX_CARDS_PER_BUNDLE, MAX_HANDLES_PER_CARD, MAX_NAME_BYTES, MAX_NOTE_BYTES,
    MAX_PENDING_SUGGESTIONS, MAX_PICTURE_BYTES, MAX_SHARED_DEVICES, MAX_TOMBSTONES,
};
pub use bundle::{CardBundle, SharedCard, BUNDLE_VERSION};
pub use codec::{encode_picture, HANDLE_KEY_VERSION, SCHEMA_VERSION};
pub use engine::{Action, BookEngine};
pub use error::Error;
pub use ids::{CardId, PathHash};
pub use mapping::BODY_KEY;
pub use registry::{Registry, RegistryError};
pub use store::Store;
pub use types::{
    Author, Book, Card, Coverage, DerivedObservation, Evidence, Field, GroupLink, Handle,
    HandleKey, HandleView, Link, Resolution, Stamp, Tag,
};

#[cfg(test)]
mod tests;
