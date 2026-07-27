//! The product's orbital World adapter (C4): the frozen contract packet, the
//! parsed state/projection layer, and the registered `IssuesWorld`.
//!
//! See `docs/plans/04-product-world-contract.md` for the normative mapping.

use std::sync::Arc;

use crate::orbital::{
    LegacyWorldCodec, WorldCall, WorldCallError, WorldPackage, WorldPackages, WorldReply,
};

pub mod lifecycle;
pub mod router;

pub use issues::{
    contract, roles, views, workflow, IssueEffect, IssueIntent, IssueQuery, IssuesWorld,
    PRODUCT_WORLD,
};
pub use router::{IssueRouter, IssuesControlAdapter, RouterFacts};

/// The issue tracker's complete compile-time World package.
///
/// Keeping semantic implementation, reviewed identity, and application control
/// adapter in one value makes this module the product composition root. The
/// daemon and SpaceBridge receive packages by injection and do not construct or
/// name IssuesWorld themselves.
pub fn package() -> WorldPackage {
    let control = Arc::new(IssuesControlAdapter);
    WorldPackage::new(
        IssuesWorld::registration(),
        Arc::new(IssuesWorld::new()),
        implementation_id(),
    )
    .with_control(control.clone())
    .with_legacy_codec(control)
}

/// Every product World bundled by the issue-tracker application.
pub fn packages() -> WorldPackages {
    WorldPackages::new().with_package(package())
}

/// Select the product World for one request emitted by this application.
///
/// Host routing consumes the explicit result and never infers a World from the
/// current directory or from daemon-global state.
pub fn request_world(request: &crate::control::Request) -> Option<replica::ids::WorldId> {
    IssueRouter::handles(request).then(contract::world_id)
}

/// Encode the issue tracker's historical typed request as a generic World call.
pub fn encode_call(request: crate::control::Request) -> Result<WorldCall, WorldCallError> {
    IssuesControlAdapter.encode_call(request)
}

/// Decode a generic Issues reply for the historical CLI/MCP/viewer surfaces.
pub fn decode_reply(reply: WorldReply) -> crate::control::Response {
    IssuesControlAdapter.decode_reply(reply)
}

/// The reviewed IssuesWorld implementation id shipped by this build.
pub fn implementation_id() -> [u8; 32] {
    IssuesWorld::implementation_descriptor()
        .id()
        .expect("canonical IssuesWorld descriptor")
}
