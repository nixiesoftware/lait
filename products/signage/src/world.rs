use replica::body::{MutationModel, Op, Schema};
use replica::frontier::ReplicaFrontier;
use runtime::world::{
    Context, Descriptor, Effect, Intent, Limits, Projection, Query, Rejection, Version, World,
};

use crate::contract::{self, SignageIntent, SignageProgram, SignageProjection, SignageQuery};

pub struct SignageWorld {
    id: replica::body::WorldId,
    schemas: Vec<Schema>,
}

impl SignageWorld {
    pub fn new() -> Self {
        Self {
            id: contract::world_id(),
            schemas: vec![Schema {
                id: contract::program_schema(),
                version: contract::PROGRAM_SCHEMA_VERSION,
                encoding: contract::program_encoding(),
                mutation: MutationModel::Atomic,
                readable_predecessors: Vec::new(),
            }],
        }
    }

    pub fn implementation_descriptor() -> runtime::world::Implementation {
        let world = Self::new();
        runtime::world::Implementation::from_registration(
            &world.descriptor(),
            1,
            *blake3::hash(b"lait.signage.policy-table.v1").as_bytes(),
            *blake3::hash(b"lait.signage.program.v1").as_bytes(),
        )
    }
}

impl Default for SignageWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl World for SignageWorld {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            id: self.id.clone(),
            implementation_version: Version(1),
            schemas: self.schemas.clone(),
            limits: Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: Vec::new(),
            find_extractors: Vec::new(),
            exec_specs: Vec::new(),
        }
    }

    fn id(&self) -> replica::body::WorldId {
        self.id.clone()
    }

    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        if intent.schema != contract::program_schema()
            || intent.schema_version != contract::PROGRAM_SCHEMA_VERSION
        {
            return Err(Rejection::UnsupportedSchema);
        }
        let intent: SignageIntent =
            serde_json::from_slice(&intent.payload).map_err(|_| Rejection::InvalidRequest)?;
        let (key, operation) = match intent {
            SignageIntent::Put { program } => {
                if !program.validate() {
                    return Err(Rejection::InvalidRequest);
                }
                let key = program.body_key().ok_or(Rejection::InvalidRequest)?;
                let value = serde_json::to_vec(&program).map_err(|_| Rejection::InvalidRequest)?;
                (key, Op::ReplaceAtomic { value })
            }
            SignageIntent::Delete { program } => (
                contract::body_key(&program).ok_or(Rejection::InvalidRequest)?,
                Op::Tombstone,
            ),
        };
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            operations: vec![(key.clone(), operation)],
            bodies: vec![key],
            effect: Vec::new(),
            declarations: Vec::new(),
            demand: contract::demand_manage(),
        })
    }

    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        if query.schema != contract::program_schema()
            || query.schema_version != contract::PROGRAM_SCHEMA_VERSION
        {
            return Err(Rejection::UnsupportedSchema);
        }
        let query: SignageQuery =
            serde_json::from_slice(&query.payload).map_err(|_| Rejection::InvalidRequest)?;
        let projection = match query {
            SignageQuery::Program { program } => SignageProjection::Program {
                program: contract::body_key(&program)
                    .and_then(|key| ctx.read_body(&key))
                    .and_then(|bytes| serde_json::from_slice::<SignageProgram>(&bytes).ok())
                    .filter(SignageProgram::validate),
            },
            SignageQuery::Programs => {
                let mut programs = ctx
                    .bodies_with_schema(&self.id, &contract::program_schema())
                    .into_iter()
                    .filter_map(|key| ctx.read_body(&key))
                    .filter_map(|bytes| serde_json::from_slice::<SignageProgram>(&bytes).ok())
                    .filter(SignageProgram::validate)
                    .collect::<Vec<_>>();
                programs
                    .sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
                SignageProjection::Programs { programs }
            }
        };
        Ok(Projection {
            schema: contract::program_schema(),
            schema_version: contract::PROGRAM_SCHEMA_VERSION,
            bytes: serde_json::to_vec(&projection).map_err(|_| Rejection::ContractViolation)?,
            frontier: ReplicaFrontier::EMPTY,
            demand: contract::demand_read(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_implementation_is_stable_and_nonzero() {
        let first = SignageWorld::implementation_descriptor().id().unwrap();
        let second = SignageWorld::implementation_descriptor().id().unwrap();
        assert_eq!(first, second);
        assert_ne!(first, [0; 32]);
    }
}
