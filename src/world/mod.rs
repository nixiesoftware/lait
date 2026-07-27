//! The product's orbital World adapter (C4): the frozen contract packet, the
//! parsed state/projection layer, and the registered `IssuesWorld`.
//!
//! See `docs/plans/04-product-world-contract.md` for the normative mapping.

use std::sync::Arc;

use crate::orbital::{WorldControlAdapter, WorldPackage, WorldPackages};

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
    WorldPackage::new(
        IssuesWorld::registration(),
        Arc::new(IssuesWorld::new()),
        implementation_id(),
    )
    .with_control(Arc::new(IssuesControlAdapter))
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
    IssuesControlAdapter
        .handles(request)
        .then(contract::world_id)
}

/// The reviewed IssuesWorld implementation id shipped by this build.
pub fn implementation_id() -> [u8; 32] {
    IssuesWorld::implementation_descriptor()
        .id()
        .expect("canonical IssuesWorld descriptor")
}
