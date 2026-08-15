//! The self-hosted Astrolabe Display coordinator.
//!
//! This module is product-neutral. It resolves a frozen package surface,
//! executes its World query through the daemon's required-Query boundary, and
//! compiles bounded host-rendered output into the receiver protocol.

mod compiler;
mod coordinator;
mod http;
mod pairing;
mod runtime;
mod store;
mod tls;

pub use compiler::{CompiledProgram, PlaybackAlignment, ProgramCompiler};
pub use coordinator::DisplayCoordinator;
pub use http::{display_http_router, serve_display_https, DisplayHttpState};
pub use pairing::{
    AuthorizedDevice, DisplayAuthorizationError, DisplayPairingService, PendingPairingView,
};
pub use runtime::DisplayRuntime;
pub use store::{
    AssignmentRecord, AssignmentSync, CoordinatorState, CoordinatorStore, DeviceRecord, SourceGrant,
};
pub use tls::{DisplayTlsIdentity, DEFAULT_DISPLAY_PORT};
