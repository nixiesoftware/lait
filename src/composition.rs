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
//! * the **client registry** mounted by every head.
//!
//! `tests/it/product_independence.rs` allowlists this file and nothing else in
//! `src/**`. That is the invariant worth keeping: the count of files naming a
//! product is one, and it is this one. When the shell becomes its own crate,
//! this file moves to the binary and the library is product-free outright.

use std::sync::Arc;

use crate::orbital::{
    BootstrapContext, FounderGrant, InitialScope, Invalidation, ObservationProjector,
    StatusProjection, WorldLifecycle, WorldPackage, WorldPackages,
};
use runtime::poison::LockRecovering;
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
        .with_lifecycle(Arc::new(IssuesLifecycle))
}

/// Translates generic runtime Observations into the Issues doorbell dirty-set.
///
/// The baseline is per-projector state, not per-call: a dirty-set is a *diff*
/// against the planes as they stood at the last ring, so losing it between
/// observations would report every plane dirty on every ring.
#[derive(Default)]
struct IssuesProjector {
    baselines: std::sync::Mutex<
        std::collections::BTreeMap<
            String,
            Option<std::collections::BTreeMap<issues::dto::CatalogScope, String>>,
        >,
    >,
}

impl ObservationProjector for IssuesProjector {
    fn status(&self, session: &runtime::Session) -> Option<StatusProjection> {
        issues_app::projections::status(session).map(|projection| StatusProjection {
            items: projection.issues,
            scopes: projection.projects,
            name: projection.name,
            description: projection.description,
        })
    }

    fn start(&self, session: &runtime::Session, space: &mechanics::ids::SpaceId) {
        let baseline = issues_app::projections::ring_state(session).map(|state| state.planes);
        self.baselines
            .lock_recovering()
            .insert(space.as_str().to_string(), baseline);
    }

    fn project(
        &self,
        session: &runtime::Session,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Invalidation {
        let mut baselines = self.baselines.lock_recovering();
        let baseline = baselines.entry(space.as_str().to_string()).or_default();
        issues_app::projections::observation(session, space, &observation.bodies, baseline)
    }
}

struct IssuesLifecycle;

impl WorldLifecycle for IssuesLifecycle {
    fn founder_grants(&self) -> anyhow::Result<Vec<FounderGrant>> {
        let policy = issues_app::lifecycle::founder_policy();
        if policy.world != PRODUCT_WORLD || policy.implementation != implementation_id() {
            return Err(anyhow::anyhow!(
                "Issues lifecycle policy does not match its bundled World package"
            ));
        }
        Ok(policy
            .grants
            .into_iter()
            .map(|grant| FounderGrant {
                capability: grant.capability,
                resource: grant.resource,
                salt: grant.salt,
            })
            .collect())
    }

    fn initial_scope(&self, display_name: &str) -> Option<InitialScope> {
        let project = issues_app::lifecycle::InitialProject::for_space(display_name);
        Some(InitialScope {
            kind: "project".into(),
            key: project.key,
            name: project.name,
        })
    }

    fn bootstrap(&self, context: BootstrapContext<'_>) -> anyhow::Result<()> {
        let initial_project = context
            .initial_scope
            .map(|scope| {
                if scope.kind != "project" {
                    return Err(anyhow::anyhow!(
                        "Issues lifecycle expected an initial project, got '{}'",
                        scope.kind
                    ));
                }
                Ok(issues_app::lifecycle::InitialProject {
                    name: scope.name.clone(),
                    key: scope.key.clone(),
                })
            })
            .transpose()?;
        issues_app::lifecycle::bootstrap_tracker(
            context.store_root,
            context.space,
            context.session,
            context.identity,
            context.device,
            context.display_name,
            initial_project,
        )
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
