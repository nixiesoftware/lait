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

//! A UI-neutral supervisor for lait daemons.
//!
//! This crate is a *library*. It owns daemon lifecycle, device registration,
//! authoritative observation and history, and it holds no opinion about how any
//! of that is drawn. Its consumer — the Astrolabe client — links it directly
//! and calls it on native Rust types; there is no serialization, no bridge and
//! no local HTTP hop between the two.
//!
//! Both ends of the lifetime are explicit calls rather than side effects of
//! some `main`: [`Supervisor::start`] constructs a supervisor and begins the
//! background observation that keeps it authoritative, and
//! [`Supervisor::shutdown`] ends both. The [`api`] module behind the `http`
//! feature is an optional diagnostics and testing adapter over the same types
//! and the same safety rules, not the way anything embeds this crate.

#[cfg(feature = "http")]
pub mod api;
mod contract;
mod driver;
pub mod heads;
mod observability;
mod registry;
mod staging;
mod supervisor;

pub use contract::schema_bundle;
pub use contract::{
    BackendEvent, Capabilities, ConnectionEvent, ConnectionEventKind, ConnectionHistoryPage,
    ConnectionSnapshot, CreateDeviceRequest, DeviceAction, DeviceSnapshot, EnvironmentSnapshot,
    EventHistoryPage, EventKind, HistoryQuery, LifecycleState, LogEntry, LogLevel, LogPage,
    LogQuery, RemoveDeviceRequest, UpdateDeviceRequest, WorkbenchSnapshot, SCHEMA_VERSION,
};
pub use contract::{
    ClientSignal, DeviceFacts, ImageFacts, ObservationHealth, ObservationState, SnapshotReason,
    WorldCall, WorldCaller,
};
pub use heads::{HeadFacts, HeadKind, HeadState, Ownership, Stopped};
pub use staging::{StagedImage, Staging};
pub use supervisor::{Config, Signals, Supervisor, SupervisorError, OBSERVATION_INTERVAL};
