//! The product's orbital World adapter (C4): the frozen contract packet, the
//! parsed state/projection layer, and the registered `IssuesWorld`.
//!
//! See `docs/plans/04-product-world-contract.md` for the normative mapping.

use std::sync::{Arc, LazyLock};

use crate::orbital::{WorldPackage, WorldPackages};
use world_interface::WorldClientRegistry;

pub mod lifecycle;

pub use issues::{
    contract, roles, views, workflow, IssueEffect, IssueIntent, IssueQuery, IssuesWorld,
    PRODUCT_WORLD,
};

/// The issue tracker's complete compile-time World package.
///
/// Keeping semantic implementation, reviewed identity, and application control
/// adapter in one value makes this module the product composition root. The
/// daemon and StationHost receive packages by injection and do not construct or
/// name IssuesWorld themselves.
pub fn package() -> WorldPackage {
    let control = Arc::new(issues_app::IssuesCallHandler);
    WorldPackage::new(Arc::new(IssuesWorld::new()), implementation_id()).with_control(control)
}

/// Every product World bundled by the issue-tracker application.
pub fn packages() -> WorldPackages {
    WorldPackages::new().with_package(package())
}

static CLIENT_PACKAGES: LazyLock<WorldClientRegistry> = LazyLock::new(|| {
    WorldClientRegistry::new()
        .with_package(issues_app::package().expect("valid bundled Issues client package"))
        .expect("non-conflicting bundled World client packages")
});

/// Every client-facing World package mounted by the navigation shell.
pub fn client_packages() -> &'static WorldClientRegistry {
    &CLIENT_PACKAGES
}

/// The reviewed IssuesWorld implementation id shipped by this build.
pub fn implementation_id() -> [u8; 32] {
    issues_app::lifecycle::implementation_id()
}
