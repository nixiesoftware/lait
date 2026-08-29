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

pub mod addressing;
pub mod contract;
pub mod fleet;
mod world;

pub use addressing::{
    AudienceLookup, Compare, Context, Match, Observations, Place, PlaceMatch, SignageAudience,
};
pub use contract::{
    product_world, world_id, AsRunEntry, AsRunIntent, AsRunProjection, AsRunQuery, AudienceIntent,
    AudienceProjection, AudienceQuery, BroadcastIntent, BroadcastProjection, BroadcastQuery,
    ChannelIntent, ChannelProjection, ChannelQuery, MediaIntent, MediaProjection, MediaQuery,
    MediaSource, PresetIntent, PresetProjection, PresetQuery, ProgramCycle, ScreenIntent,
    ScreenProjection, ScreenQuery, Settings, SignageAsRun, SignageIntent, SignageItem,
    SignageMedia, SignagePreset, SignageProgram, SignageProjection, SignageQuery, SignageWindow,
};
pub use fleet::{
    Action, ChannelWindow, Playback, Resolved, Showing, SignageBroadcast, SignageChannel,
    SignageScreen, Timing,
};
pub use world::SignageWorld;
