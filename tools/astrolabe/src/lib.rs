//! Astrolabe — the local client through which a person reaches the Worlds their
//! device serves.
//!
//! The reference shape is the Steam client: a library, a launcher, an identity
//! and a social client that never draws the game. `products/issues` ships its
//! own head and stays the authority on its own presentation; `Open` here is a
//! handoff, not a render.
//!
//! ## What is where
//!
//! - [`client`] is the reach: the supervisor library this crate embeds, and the
//!   host, Space and World planes it speaks. It draws nothing, which is what
//!   lets every rule in it be tested without a window.
//! - [`model`] is the App-owned state. One entity consumes the ordered
//!   `ClientSignal` stream and is the *only* model of client state — nothing
//!   mirrors it and nothing derives a parallel copy.
//! - [`ui`] draws that model and holds no logic of its own.
//!
//! There is no boundary between them: no FFI, no local HTTP hop, no generated
//! binding and no serialization on the path between what is observed and what
//! is drawn. They are Rust modules calling each other on native types.

pub mod client;
pub mod lifecycle;
pub mod link;
pub mod model;
pub mod runtime;
pub mod sidecar;
pub mod single_instance;
pub mod ui;

pub use client::{Client, ClientError, ClientResult, Config};
pub use model::App;
