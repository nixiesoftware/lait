//! The product's orbital World adapter (C4): the frozen contract packet, the
//! parsed state/projection layer, and the registered `IssuesWorld`.
//!
//! See `docs/plans/04-product-world-contract.md` for the normative mapping.

use std::sync::{Arc, LazyLock};

use crate::orbital::{
    Invalidation, ObservationProjector, StatusProjection, WorldPackage, WorldPackages,
};
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
    let projector = Arc::new(IssuesProjector::default());
    WorldPackage::new(Arc::new(IssuesWorld::new()), implementation_id())
        .with_control(control)
        .with_projector(projector)
}

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

/// Every product World bundled by the issue-tracker application.
pub fn packages() -> WorldPackages {
    WorldPackages::new().with_package(package())
}

static CLIENT_PACKAGES: LazyLock<WorldClientRegistry> = LazyLock::new(|| {
    issues_app::package()
        .ok()
        .and_then(|package| WorldClientRegistry::new().with_package(package).ok())
        .unwrap_or_default()
});

/// Every client-facing World package mounted by the navigation shell.
pub fn client_packages() -> &'static WorldClientRegistry {
    &CLIENT_PACKAGES
}

/// The reviewed IssuesWorld implementation id shipped by this build.
pub fn implementation_id() -> [u8; 32] {
    issues_app::lifecycle::implementation_id()
}
