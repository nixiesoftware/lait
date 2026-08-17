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
    let spec = issues::contract::verify_spec();
    let build = issues::contract::verify_build(implementation_id());
    let exec = runtime::exec::Package::new()
        .with_spec(spec)
        .with_build(build.clone())
        .with_handler(issues::handler::verify_handler(&build));
    WorldPackage::new(Arc::new(IssuesWorld::new()), implementation_id())
        .with_control(control)
        .with_exec(exec)
        .with_projector(projector)
        .with_lifecycle(Arc::new(IssuesLifecycle))
}

/// The Signage World's host-side semantic package. The shell only sees the
/// generic package value; Signage-specific protocol and rendering stay in the
/// product crates named by this composition root.
pub fn signage_package() -> WorldPackage {
    WorldPackage::new(
        Arc::new(signage::SignageWorld::new()),
        signage_app::implementation_id(),
    )
    .with_control(Arc::new(signage_app::SignageCallHandler))
    .with_projector(Arc::new(SignageProjector))
    .with_lifecycle(Arc::new(SignageLifecycle))
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

struct SignageLifecycle;

struct SignageProjector;

impl ObservationProjector for SignageProjector {
    fn status(&self, session: &runtime::Session) -> Option<StatusProjection> {
        let projection = session
            .query(runtime::world::Query {
                schema: signage::contract::program_schema(),
                schema_version: signage::contract::PROGRAM_SCHEMA_VERSION,
                payload: serde_json::to_vec(&signage::SignageQuery::Programs).ok()?,
                publication: None,
            })
            .ok()?;
        let signage::SignageProjection::Programs { programs } =
            serde_json::from_slice(&projection.bytes).ok()?
        else {
            return None;
        };
        Some(StatusProjection {
            items: programs.len(),
            scopes: usize::from(!programs.is_empty()),
            name: "Signage".into(),
            description: "Durable display programs".into(),
        })
    }

    fn start(&self, _session: &runtime::Session, _space: &mechanics::ids::SpaceId) {}

    fn project(
        &self,
        _session: &runtime::Session,
        _space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Invalidation {
        if observation
            .bodies
            .iter()
            .any(|key| key.world == signage::contract::world_id())
        {
            Invalidation {
                dirty: Vec::new(),
                planes: vec![runtime::world::DirtyPlane {
                    plane: "programs".into(),
                    scope: None,
                }],
            }
        } else {
            Invalidation::default()
        }
    }
}

impl WorldLifecycle for SignageLifecycle {
    fn founder_grants(&self) -> anyhow::Result<Vec<FounderGrant>> {
        Ok(signage::contract::founder_capabilities()
            .into_iter()
            .enumerate()
            .map(|(index, (capability, resource))| FounderGrant {
                capability,
                resource,
                salt: [u8::try_from(index).unwrap_or(u8::MAX); 16],
            })
            .collect())
    }

    fn initial_scope(&self, _display_name: &str) -> Option<InitialScope> {
        None
    }

    fn bootstrap(&self, _context: BootstrapContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}

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
    WorldPackages::new()
        .with_package(package())
        .with_package(signage_package())
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
        .and_then(|issues| WorldClientRegistry::new().with_package(issues).ok())
        .and_then(|registry| {
            signage_app::package()
                .ok()
                .and_then(|signage| registry.with_package(signage).ok())
        })
        .unwrap_or_default()
}

/// The reviewed IssuesWorld implementation id shipped by this build.
pub fn implementation_id() -> [u8; 32] {
    issues_app::lifecycle::implementation_id()
}
