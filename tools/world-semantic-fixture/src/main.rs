use replica::body::{BodyId, BodyKey, EncodingId, MutationModel, Schema, SchemaId, WorldId};
use runtime::world::{Context, Effect, Intent, Projection, Query, Rejection, World};

struct FixtureWorld {
    id: WorldId,
    body: BodyId,
    schemas: Vec<Schema>,
}

impl FixtureWorld {
    fn new() -> Self {
        Self {
            id: WorldId::parse("com.lait.semantic-fixture").expect("fixture World id"),
            body: BodyId::from_bytes([0x51; 16]),
            schemas: vec![Schema {
                id: SchemaId::parse("record").expect("fixture schema"),
                version: 1,
                encoding: EncodingId::parse("fixture.bytes").expect("fixture encoding"),
                mutation: MutationModel::Atomic,
                readable_predecessors: Vec::new(),
            }],
        }
    }

    fn body(&self) -> BodyKey {
        BodyKey::new(self.id.clone(), self.body.clone())
    }
}

impl World for FixtureWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }

    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    fn submit(&self, _context: &mut Context<'_>, _intent: Intent) -> Result<Effect, Rejection> {
        Err(Rejection::InvalidRequest)
    }

    fn query(&self, context: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        let bytes = context
            .read_body(&self.body())?
            .ok_or(Rejection::StateCorrupt)?;
        Ok(Projection {
            schema: self.schemas[0].id.clone(),
            schema_version: 1,
            bytes: bytes.to_vec(),
            frontier: replica::frontier::ReplicaFrontier::EMPTY,
            publication: None,
            demand: vec![1],
        })
    }
}

fn main() -> anyhow::Result<()> {
    let version = std::env::var("LAIT_WORLD_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
    world_runner::serve(
        "com.lait.semantic-fixture",
        version,
        world_sdk::WorldService::new(FixtureWorld::new(), [0x61; 32]),
    )
}
