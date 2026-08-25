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
#[cfg(test)]
mod stored_tests;

pub use http::serve_display_on;
/// The two the daemon needs to tell "the port is taken" from "our service
/// broke". Re-exported rather than opening the module: everything else in there
/// is the HTTPS surface itself, which has one caller.
pub(crate) use http::{bind_display, is_port_taken};
mod live;
pub mod overlay;
mod pairing;
mod pin;
mod publish;
mod runtime;
mod store;
mod tls;

pub use cmaf::{
    CmafCatalogPackager, CmafFragment, CmafRenditionFragment, CmafTrackDescription,
    CmafTrackPackager, Failure as CmafFailure,
};
pub use compiler::{CompiledProgram, PlaybackAlignment, ProgramCompiler};
pub use coordinator::{DisplayCoordinator, SurfaceRender};
pub use hls::{Failure as HlsFailure, HlsCatalogPackager, HlsRenditionDescription, HlsSegment};
pub use http::{display_http_router, serve_display_https, DisplayHttpState};
pub use live::{LiveMediaHub, LiveMediaPacket, LiveMediaSnapshot, LiveMediaTrack, LiveTransport};
pub use mediabox::{
    read_catalog, track_shapes, CatalogPolicy, Failure as DemuxFailure, SegmentPlan, StoredMedia,
    StoredPlan, TrackShape,
};
pub use pairing::{
    AuthorizationRefusal, AuthorizedDevice, DisplayPairingService, PendingPairingView,
};
pub use publish::publish_route;
pub use runtime::DisplayRuntime;
pub use store::{
    AssignmentRecord, AssignmentSync, CoordinatorPolicy, CoordinatorSecrets, CoordinatorStore,
    Custodian, DeviceRecord, IdentifierCustody, IdentifierCustodyStatus, SourceGrant,
};
pub use tls::{DisplayTlsIdentity, DEFAULT_DISPLAY_PORT};
