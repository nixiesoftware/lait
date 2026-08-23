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
//! - [`api`] is the boundary: the whole of what the interface can know, and
//!   the whole of what it can ask for.
//!
//! ## There is one interface, and it is Tauri
//!
//! `apps/astrolabe-web` (TypeScript/React over `src-tauri`) is the canonical
//! and only live interface. Its host links this crate directly and takes
//! [`api::ClientView`] apart by exhaustive destructuring, so the boundary is
//! checked by the compiler rather than by a generator.
//!
//! Two interfaces preceded it and both are gone from the live build. The egui
//! one was deleted outright. The Flutter one (`apps/astrolabe`) is
//! **deprecated and unwired**: its `flutter_rust_bridge` dependency, the
//! checked-in generated binding, and every `#[frb]` annotation on [`api`] have
//! been removed from this crate, so no build here compiles a line of it. See
//! `apps/astrolabe/DEPRECATED.md`.
//!
//! The rule that survived all three: [`model::App`] is the only model of
//! client state. An interface receives whole immutable projections of it and
//! holds nothing but drafts.

pub mod api;
pub mod browser;
pub mod client;
pub mod lifecycle;
pub mod link;
pub mod model;
pub mod notify;
pub mod runtime;
pub mod screen;
pub mod sidecar;
pub mod single_instance;
pub mod tray;

pub use client::{Client, ClientError, ClientResult, Config};
pub use model::App;
