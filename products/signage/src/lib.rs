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

pub mod contract;
mod world;

pub use contract::{
    ProgramCycle, SignageIntent, SignageItem, SignageProgram, SignageProjection, SignageQuery,
    PRODUCT_WORLD,
};
pub use world::SignageWorld;
