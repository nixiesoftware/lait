//! The shell's World-hosting surface: which packages this application mounts,
//! for the host side and the client side.
//!
//! It names no World. The bundled set comes from [`crate::composition`], the
//! single file that knows which products this build ships — so adding,
//! removing, or swapping a World is an edit there and nowhere else.
//!
//! See `docs/plans/04-product-world-contract.md` for the normative mapping.

use std::sync::LazyLock;

use crate::orbital::WorldPackages;
use world_interface::WorldClientRegistry;

pub mod lifecycle;

// The product's semantic vocabulary, re-exported through the module the rest of
// the shell already reaches for. The names cross this seam; the dependency does
// not — every one of them resolves through the composition root, so swapping the
// bundled World changes one file rather than every call site.
pub use crate::composition::{
    contract, implementation_id, package, roles, views, workflow, IssueEffect, IssueIntent,
    IssueQuery, IssuesWorld, PRODUCT_WORLD,
};

/// Every product World bundled by this application, for the host side.
pub fn packages() -> WorldPackages {
    crate::composition::bundled_packages()
}

/// Built once per process: a registry is a fixed property of the build, and the
/// CLI, MCP, and web surfaces all resolve mounts against the same one.
static CLIENT_PACKAGES: LazyLock<WorldClientRegistry> =
    LazyLock::new(crate::composition::bundled_client_packages);

/// Every client-facing World package mounted by the navigation shell.
pub fn client_packages() -> &'static WorldClientRegistry {
    &CLIENT_PACKAGES
}
