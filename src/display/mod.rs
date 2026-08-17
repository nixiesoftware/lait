//! The self-hosted Astrolabe Display coordinator.
//!
//! This module is product-neutral. It resolves a frozen package surface,
//! executes its World query through the daemon's required-Query boundary, and
//! compiles bounded host-rendered output into the receiver protocol.

mod cmaf;
mod compiler;
mod coordinator;
mod hls;
mod http;
mod live;
mod pairing;
mod runtime;
mod store;
mod tls;

pub use cmaf::{
    CmafCatalogPackager, CmafFragment, CmafRenditionFragment, CmafTrackDescription,
    CmafTrackPackager, Failure as CmafFailure,
};
pub use compiler::{CompiledProgram, PlaybackAlignment, ProgramCompiler};
pub use coordinator::DisplayCoordinator;
pub use hls::{Failure as HlsFailure, HlsCatalogPackager, HlsRenditionDescription, HlsSegment};
pub use http::{display_http_router, serve_display_https, DisplayHttpState};
pub use live::{LiveMediaHub, LiveMediaPacket, LiveMediaSnapshot, LiveMediaTrack, LiveTransport};
pub use pairing::{
    AuthorizationRefusal, AuthorizedDevice, DisplayPairingService, PendingPairingView,
};
pub use runtime::DisplayRuntime;
pub use store::{
    AssignmentRecord, AssignmentSync, CoordinatorPolicy, CoordinatorSecrets, CoordinatorStore,
    DeviceRecord, SourceGrant,
};
pub use tls::{DisplayTlsIdentity, DEFAULT_DISPLAY_PORT};
