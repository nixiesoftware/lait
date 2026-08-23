//! Formation, migration, and observation behavior served by the Issues runner.

use std::collections::BTreeMap;
use std::sync::Mutex;

use runtime::poison::LockRecovering;
use world_sdk::{
    BootstrapContext, FounderGrant, InitialScope, ReviewedImplementation, StatusProjection,
    WorldApplication, WorldUpgradeAssessment, WorldUpgradeContext, WorldUpgradeProgress,
};

#[derive(Default)]
pub struct IssuesApplication {
    baselines: Mutex<BTreeMap<String, Option<BTreeMap<issues::dto::CatalogScope, String>>>>,
}

impl WorldApplication for IssuesApplication {
    fn founder_grants(&self) -> anyhow::Result<Vec<FounderGrant>> {
        let policy = crate::lifecycle::founder_policy();
        if policy.world != issues::PRODUCT_WORLD
            || policy.implementation != crate::lifecycle::implementation_id()
        {
            anyhow::bail!("Issues lifecycle policy does not match its World release");
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

    fn admission_evidence(
        &self,
        role: &str,
        parent_manifest_root: [u8; 32],
    ) -> anyhow::Result<Option<mechanics::authorization::WorldAssignmentEvidence>> {
        let Some(role_id) = issues::roles::resolve_role_selector(role) else {
            return Ok(None);
        };
        let Some(revision) = issues::roles::built_in(role_id) else {
            return Ok(None);
        };
        if revision.body.tombstone {
            anyhow::bail!("Issues role `{role_id}` is tombstoned");
        }
        Ok(Some(issues::roles::role_admission_evidence(
            &revision,
            parent_manifest_root,
        )))
    }

    fn initial_scope(&self, display_name: &str) -> Option<InitialScope> {
        let project = crate::lifecycle::InitialProject::for_space(display_name);
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
                    anyhow::bail!(
                        "Issues lifecycle expected an initial project, got '{}'",
                        scope.kind
                    );
                }
                Ok(crate::lifecycle::InitialProject {
                    name: scope.name.clone(),
                    key: scope.key.clone(),
                })
            })
            .transpose()?;
        crate::lifecycle::bootstrap_tracker(
            context.store_root,
            context.space,
            context.session,
            context.identity,
            context.device,
            context.display_name,
            initial_project,
        )
    }

    fn assess_upgrade(
        &self,
        active: Option<ReviewedImplementation>,
        preferred: ReviewedImplementation,
    ) -> anyhow::Result<WorldUpgradeAssessment> {
        use crate::lifecycle::{ImplementationCoordinate, UpgradeAssessment};
        let coordinate = |value: ReviewedImplementation| ImplementationCoordinate {
            id: value.id,
            version: value.version,
        };
        let reviewed = |value: ImplementationCoordinate| ReviewedImplementation {
            id: value.id,
            version: value.version,
        };
        Ok(
            match crate::lifecycle::assess_upgrade(active.map(coordinate), coordinate(preferred)) {
                UpgradeAssessment::Current => WorldUpgradeAssessment::Current,
                UpgradeAssessment::Direct => WorldUpgradeAssessment::Direct,
                UpgradeAssessment::ConsentRequired { migrator } => {
                    WorldUpgradeAssessment::ConsentRequired {
                        migrator: reviewed(migrator),
                    }
                }
                UpgradeAssessment::InProgress { migrator } => WorldUpgradeAssessment::InProgress {
                    migrator: reviewed(migrator),
                },
                UpgradeAssessment::Unsupported { reason } => {
                    WorldUpgradeAssessment::Unsupported { reason }
                }
            },
        )
    }

    fn verification_migrator(
        &self,
        _preferred: ReviewedImplementation,
    ) -> Option<ReviewedImplementation> {
        let migrator = crate::lifecycle::migrator_implementation();
        Some(ReviewedImplementation {
            id: migrator.id,
            version: migrator.version,
        })
    }

    fn upgrade_step(
        &self,
        context: WorldUpgradeContext<'_>,
    ) -> anyhow::Result<WorldUpgradeProgress> {
        use crate::lifecycle::{ImplementationCoordinate, UpgradeProgress};
        let coordinate = |value: ReviewedImplementation| ImplementationCoordinate {
            id: value.id,
            version: value.version,
        };
        let progress = crate::lifecycle::upgrade_step(crate::lifecycle::UpgradeContext {
            space: context.space,
            session: context.session,
            identity: context.identity,
            device: context.device,
            active: coordinate(context.active),
            migrator: coordinate(context.migrator),
            preferred: coordinate(context.preferred),
            source: context.source,
            record: context.record,
        })?;
        Ok(match progress {
            UpgradeProgress::Pending {
                completed,
                remaining,
                record,
            } => WorldUpgradeProgress::Pending {
                completed,
                remaining,
                record,
            },
            UpgradeProgress::Verified { record } => WorldUpgradeProgress::Verified { record },
        })
    }

    fn status(
        &self,
        session: &dyn runtime::world::call::SessionAccess,
    ) -> Option<StatusProjection> {
        crate::projections::status(session).map(|projection| StatusProjection {
            items: projection.issues,
            scopes: projection.projects,
            name: projection.name,
            description: projection.description,
        })
    }

    fn start_projector(
        &self,
        session: &dyn runtime::world::call::SessionAccess,
        space: &mechanics::ids::SpaceId,
    ) {
        let baseline = crate::projections::ring_state(session).map(|state| state.planes);
        self.baselines
            .lock_recovering()
            .insert(space.as_str().to_string(), baseline);
    }

    fn project(
        &self,
        session: &dyn runtime::world::call::SessionAccess,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> runtime::world::Invalidation {
        let mut baselines = self.baselines.lock_recovering();
        let baseline = baselines.entry(space.as_str().to_string()).or_default();
        crate::projections::observation(session, space, &observation.bodies, baseline)
    }
}
