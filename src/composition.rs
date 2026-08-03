//! The application composition root: the one place that names a product.
//!
//! Everything else in the shell hosts *Worlds* without knowing which. This file
//! knows exactly one thing the rest does not — that this build bundles the
//! Issues World — and it exists so that knowledge sits in a single named place
//! instead of being spread through the machinery.
//!
//! It holds the three things only a composition root can:
//!
//! * the **projector**, which must live shell-side because
//!   [`ObservationProjector`] is declared here rather than in an engine crate.
//!   Until that trait moves, no out-of-tree World can supply its own, so the
//!   binding has to be written by whoever owns both halves — which is this file.
//! * the **package set** handed to the daemon and StationHost.
//! * the **client registry** mounted by the CLI, MCP, and web surfaces.
//!
//! `tests/it/product_independence.rs` allowlists this file and nothing else in
//! `src/**`. That is the invariant worth keeping: the count of files naming a
//! product is one, and it is this one. When the shell becomes its own crate,
//! this file moves to the binary and the library is product-free outright.

use std::sync::Arc;

use crate::orbital::{
    Invalidation, ObservationProjector, StatusProjection, WorldPackage, WorldPackages,
};
use world_interface::WorldClientRegistry;

pub use issues::{
    contract, roles, views, workflow, IssueEffect, IssueIntent, IssueQuery, IssuesWorld,
    PRODUCT_WORLD,
};

/// The issue tracker's complete compile-time World package.
///
/// Keeping semantic implementation, reviewed identity, and application control
/// adapter in one value is what lets the daemon and StationHost receive
/// packages by injection and never name a World themselves.
pub fn package() -> WorldPackage {
    let control = Arc::new(issues_app::IssuesCallHandler);
    let projector = Arc::new(IssuesProjector::default());
    WorldPackage::new(Arc::new(IssuesWorld::new()), implementation_id())
        .with_control(control)
        .with_projector(projector)
}

/// Translates generic runtime Observations into the Issues doorbell dirty-set.
///
/// The baseline is per-projector state, not per-call: a dirty-set is a *diff*
/// against the planes as they stood at the last ring, so losing it between
/// observations would report every plane dirty on every ring.
#[derive(Default)]
struct IssuesProjector {
    baseline:
        std::sync::Mutex<Option<std::collections::BTreeMap<issues::dto::CatalogScope, String>>>,
}

impl ObservationProjector for IssuesProjector {
    fn status(&self, session: &runtime::Session) -> Option<StatusProjection> {
        issues_app::projections::status(session).map(|projection| StatusProjection {
            items: projection.issues,
            groups: projection.projects,
            name: projection.name,
            description: projection.description,
        })
    }

    fn start(&self, session: &runtime::Session, _space: &mechanics::ids::SpaceId) {
        let baseline = issues_app::projections::ring_state(session).map(|state| state.planes);
        *self
            .baseline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = baseline;
    }

    fn project(
        &self,
        session: &runtime::Session,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Invalidation {
        let mut baseline = self
            .baseline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        issues_app::projections::observation(session, space, &observation.bodies, &mut baseline)
    }
}

/// Every product World this build bundles, for the host side.
pub fn bundled_packages() -> WorldPackages {
    WorldPackages::new().with_package(package())
}

/// Every client-facing World package this build mounts.
///
/// A registration failure degrades to an empty registry rather than panicking
/// here, because the collision it would report is a programming error the
/// `product_independence` and mount-validation gates catch at build time — not
/// something a user can cause at runtime.
pub fn bundled_client_packages() -> WorldClientRegistry {
    issues_app::package()
        .ok()
        .and_then(|package| WorldClientRegistry::new().with_package(package).ok())
        .unwrap_or_default()
}

/// The reviewed IssuesWorld implementation id shipped by this build.
pub fn implementation_id() -> [u8; 32] {
    issues_app::lifecycle::implementation_id()
}

/// The bundled product's Space-lifecycle bindings: founder policy, bootstrap
/// tracking, and the initial scope a freshly founded Space is seeded with.
///
/// Re-exported as a module rather than wrapped function by function, because
/// this is a staging post, not a destination. `crate::world::lifecycle` still
/// names three of these as *types* in its own signatures
/// (`-> Option<IssuesBootstrapRecord>`, `InitialProject`, `BootstrapPhase`), and
/// no amount of moving fixes that: a shell function whose return type is a
/// product struct is product-shaped whichever module the path points at.
///
/// Closing that needs the World contract to carry lifecycle hooks — a generic
/// bootstrap record and a generic "initial scope" a World declares for itself —
/// which is a design change to the seam, not a relocation. This module is the
/// complete, greppable list of what such a hook API has to cover.
pub use issues_app::lifecycle as product_lifecycle;
