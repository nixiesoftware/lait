//! Formation and projection behavior served by the Signage runner.

use world_sdk::{BootstrapContext, FounderGrant, StatusProjection, WorldApplication};

pub struct SignageApplication;

impl WorldApplication for SignageApplication {
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

    fn bootstrap(&self, _context: BootstrapContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    fn status(
        &self,
        session: &dyn runtime::world::call::SessionAccess,
    ) -> Option<StatusProjection> {
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

    fn project(
        &self,
        _session: &dyn runtime::world::call::SessionAccess,
        _space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> runtime::world::Invalidation {
        if observation
            .bodies
            .iter()
            .any(|key| key.world == signage::contract::world_id())
        {
            runtime::world::Invalidation {
                dirty: Vec::new(),
                planes: vec![runtime::world::DirtyPlane {
                    plane: "programs".into(),
                    scope: None,
                }],
            }
        } else {
            runtime::world::Invalidation::default()
        }
    }
}
