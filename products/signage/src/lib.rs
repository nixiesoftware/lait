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

//! The durable Signage World. A program authored here is the source of truth;
//! display coordinators only query a bounded window and render it.

/// Immutable release version carried by this product package.
pub const RELEASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod contract;
mod world;

pub use contract::{
    product_world, ConfigIntent, ConfigProjection, ConfigQuery, GroupIntent, GroupProjection,
    GroupQuery, MediaIntent, MediaProjection, MediaQuery, MediaSource, Playback, PlaybackSource,
    ProgramCycle, ProgramWindow, ScreenIntent, ScreenProjection, ScreenQuery, Settings,
    SignageConfig, SignageGroup, SignageIntent, SignageItem, SignageMedia, SignageProgram,
    SignageProjection, SignageQuery, SignageScreen, SignageWindow,
};
pub use world::SignageWorld;
