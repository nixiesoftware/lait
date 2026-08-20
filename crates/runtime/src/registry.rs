#![allow(
    clippy::as_conversions,
    reason = "registry schema counts are validated against public registration limits"
)]
//! The immutable World registry.
//!
//! World implementations register with a [`Builder`]. `build()` validates
//! and **freezes** the set: descriptors are immutable per Runtime, and dynamic
//! loading is deferred. Runtime rejects duplicate ids, duplicate schema
//! versions within a World, invalid limits, contradictory upgrade claims, and
//! scope/signal declarations that repeat a name, bound it at zero, exceed the
//! substrate ceiling they may only tighten, or carry demand bytes that are not
//! canonical. Find declarations additionally require every Body source to
//! exist and to have exactly one package extractor binding. Exec declarations
//! must be canonical and every embedded Find Grant must be contained by those
//! active Find declarations.

use std::collections::BTreeMap;
use std::sync::Arc;

use replica::body::WorldId;

use crate::{
    exec::{self, SchemaRef as ExecSchemaRef},
    find::{Extractor, SchemaRef, SourceRef},
    world::{Descriptor, World},
};

/// Which declaration list a registration failure is about.
///
/// One discriminated pair of variants rather than four near-identical ones:
/// the two lists fail for the same reasons and a caller that wants to report
/// them differently already has the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Declaration {
    Scope,
    Signal,
}

/// Why registration was rejected at `build()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Application composition installed multiple implementations for one
    /// World without selecting exactly one formation/default package.
    AmbiguousWorldDefault(WorldId),
    /// Two Worlds registered the same [`WorldId`].
    DuplicateWorld(WorldId),
    /// A World declared the same `(schema id, version)` twice.
    DuplicateSchemaVersion {
        world: WorldId,
        schema: String,
        version: u32,
    },
    /// A World attempted to claim a Runtime-owned Exec Body schema.
    ReservedSchema { world: WorldId, schema: String },
    /// A schema claims to read a predecessor version that is not strictly older
    /// than itself, or lists the same predecessor twice.
    ContradictoryUpgrade {
        world: WorldId,
        schema: String,
        version: u32,
    },
    /// A World declared an invalid limit.
    InvalidLimits(WorldId),
    /// A World declared the same scope or signal name twice.
    DuplicateDeclaration {
        world: WorldId,
        kind: Declaration,
        name: String,
    },
    /// A declaration's bound is zero, exceeds the substrate ceiling it may only
    /// tighten, or its demand is not canonical.
    InvalidDeclaration {
        world: WorldId,
        kind: Declaration,
        name: String,
    },
    /// Two Find declarations use the same Schema coordinate.
    DuplicateFindSchema { world: WorldId, schema: SchemaRef },
    /// A Find declaration is non-canonical, internally cross-wired, or names
    /// an undeclared target Find Schema.
    InvalidFindDeclaration { world: WorldId, schema: SchemaRef },
    /// A Find Schema names a Body Schema version this World does not declare.
    MissingFindSource {
        world: WorldId,
        schema: SchemaRef,
        source: SourceRef,
    },
    /// Two package extractors claim the same exact coordinates.
    DuplicateFindExtractor {
        world: WorldId,
        extractor: Extractor,
    },
    /// A declared source has no package extractor at the same coordinates.
    MissingFindExtractor {
        world: WorldId,
        extractor: Extractor,
    },
    /// A package extractor has no declaration at the same coordinates.
    ExtraFindExtractor {
        world: WorldId,
        extractor: Extractor,
    },
    /// The descriptor's reviewed Find declaration disagrees with the package
    /// methods that supply it.
    FindRegistrationMismatch(WorldId),
    /// Two callable Exec Specs use the same exact coordinate.
    DuplicateExecSpec { world: WorldId, spec: ExecSchemaRef },
    /// The Exec Spec list cannot be represented by its canonical count word.
    TooManyExecSpecs { world: WorldId, count: usize },
    /// An Exec Spec is invalid or widens the active World Find declaration.
    InvalidExecSpec { world: WorldId, spec: ExecSchemaRef },
    /// The descriptor's reviewed Exec declaration disagrees with the package
    /// method that supplies it.
    ExecRegistrationMismatch(WorldId),
    /// The application-owned executable package disagrees with the reviewed
    /// World or contains an ambiguous local binding.
    InvalidExecPackage {
        world: WorldId,
        reason: exec::PackageInvalid,
    },
}

/// Declarations one descriptor section may carry.
///
/// The section's entry count is a `u16` on the wire, so this is what that type
/// can say. A World past it would encode a count that wrapped and derive an
/// implementation id over bytes nothing can decode.
pub const MAX_DECLARATIONS_PER_SECTION: usize = u16::MAX as usize;

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Refusal {}

/// One hosted World: its descriptor and implementation.
struct Hosted {
    descriptor: Descriptor,
    world: Arc<dyn World>,
    /// Authority-reviewed package identity. `None` exists only for the
    /// low-level Runtime builder used by embedders and tests; application
    /// composition always registers the reviewed identity.
    reviewed_implementation: Option<[u8; 32]>,
}

/// The frozen, immutable set of hosted World implementations. Authority
/// selects the active implementation; retained publications may resolve any
/// other exact package still installed in this catalog.
#[derive(Clone)]
pub struct Catalog {
    worlds: Arc<BTreeMap<WorldId, BTreeMap<Option<[u8; 32]>, Arc<Hosted>>>>,
}

// `Hosted` holds `Arc<dyn World>`, which is not `Debug`; the registry only ever
// shows which Worlds it hosts, so `Debug` lists the ids rather than deriving.
impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("worlds", &self.worlds.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Catalog {
    /// The number of hosted Worlds.
    pub fn len(&self) -> usize {
        self.worlds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.worlds.is_empty()
    }

    /// Whether a World is hosted.
    pub fn contains(&self, id: &WorldId) -> bool {
        self.worlds.contains_key(id)
    }

    /// The implementation for a hosted World, if any.
    pub fn world(&self, id: &WorldId) -> Option<Arc<dyn World>> {
        let implementations = self.worlds.get(id)?;
        (implementations.len() == 1).then(|| {
            implementations
                .values()
                .next()
                .map(|hosted| hosted.world.clone())
        })?
    }

    /// Resolve executable code for the exact implementation authority made
    /// active. A reviewed package is never returned under a different id.
    pub fn world_for(&self, id: &WorldId, implementation: [u8; 32]) -> Option<Arc<dyn World>> {
        let implementations = self.worlds.get(id)?;
        implementations
            .get(&Some(implementation))
            .map(|hosted| hosted.world.clone())
    }

    /// Resolve the explicitly unreviewed embedder registration, if present.
    ///
    /// This is deliberately separate from [`Self::world_for`]: an unreviewed
    /// package has no authority-reviewed implementation coordinate and must
    /// never be made to impersonate an arbitrary digest. Runtime's low-level
    /// embedding mode may use this for its current Session only; retained
    /// publication lookup is always exact.
    pub(crate) fn unreviewed_world(&self, id: &WorldId) -> Option<Arc<dyn World>> {
        self.worlds
            .get(id)?
            .get(&None)
            .map(|hosted| hosted.world.clone())
    }

    /// The descriptor for a World with exactly one installed implementation.
    /// Call [`Self::descriptor_for`] when resolving authority or a retained
    /// publication coordinate.
    pub fn descriptor(&self, id: &WorldId) -> Option<&Descriptor> {
        let implementations = self.worlds.get(id)?;
        (implementations.len() == 1).then(|| {
            implementations
                .values()
                .next()
                .map(|hosted| &hosted.descriptor)
        })?
    }

    /// Resolve the descriptor at one exact authority-reviewed implementation.
    pub fn descriptor_for(&self, id: &WorldId, implementation: [u8; 32]) -> Option<&Descriptor> {
        let implementations = self.worlds.get(id)?;
        implementations
            .get(&Some(implementation))
            .map(|hosted| &hosted.descriptor)
    }

    /// Descriptor paired with [`Self::unreviewed_world`]. It has no exact
    /// implementation identity and is therefore not a historical resolver.
    pub(crate) fn unreviewed_descriptor(&self, id: &WorldId) -> Option<&Descriptor> {
        self.worlds
            .get(id)?
            .get(&None)
            .map(|hosted| &hosted.descriptor)
    }

    /// Every installed descriptor for a World, ordered by exact implementation
    /// identity. Used at activation to declare the union of readable Body
    /// schemas without choosing an implementation implicitly.
    pub fn descriptors(&self, id: &WorldId) -> impl Iterator<Item = &Descriptor> {
        self.worlds
            .get(id)
            .into_iter()
            .flat_map(|implementations| implementations.values())
            .map(|hosted| &hosted.descriptor)
    }

    /// The hosted World ids, in canonical order.
    pub fn ids(&self) -> impl Iterator<Item = &WorldId> {
        self.worlds.keys()
    }
}

/// Accumulates World registrations, then validates and freezes them into an
/// immutable [`Catalog`]. Consumed by `build()`.
#[derive(Default)]
pub struct Builder {
    pending: Vec<Hosted>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a World. Its descriptor is obtained from the implementation;
    /// validation is deferred to [`Builder::build`], so ordering never
    /// masks a duplicate.
    pub fn register(mut self, world: Arc<dyn World>) -> Self {
        let descriptor = world.descriptor();
        self.pending.push(Hosted {
            descriptor,
            world,
            reviewed_implementation: None,
        });
        self
    }

    /// Register executable code under the exact authority-reviewed identity.
    pub fn register_reviewed(
        mut self,
        world: Arc<dyn World>,
        reviewed_implementation: [u8; 32],
    ) -> Self {
        let descriptor = world.descriptor();
        self.pending.push(Hosted {
            descriptor,
            world,
            reviewed_implementation: Some(reviewed_implementation),
        });
        self
    }

    /// Validate and freeze the registry. Rejects duplicate Worlds/schema
    /// versions, registration/impl mismatch, invalid limits, contradictory
    /// upgrade claims, and invalid or duplicated scope/signal declarations.
    pub fn build(self) -> Result<Catalog, Refusal> {
        let mut worlds: BTreeMap<WorldId, BTreeMap<Option<[u8; 32]>, Arc<Hosted>>> =
            BTreeMap::new();
        let mut body_contracts: BTreeMap<
            (WorldId, replica::body::SchemaId, u32),
            replica::body::Schema,
        > = BTreeMap::new();
        for hosted in self.pending {
            let id = hosted.descriptor.id.clone();

            if hosted.world.find_schemas() != hosted.descriptor.find_schemas.as_slice()
                || hosted.world.find_extractors() != hosted.descriptor.find_extractors.as_slice()
            {
                return Err(Refusal::FindRegistrationMismatch(id));
            }
            if hosted.world.exec_specs() != hosted.descriptor.exec_specs.as_slice() {
                return Err(Refusal::ExecRegistrationMismatch(id));
            }

            // Per-World schema validation.
            let mut seen_versions: std::collections::BTreeSet<(String, u32)> =
                std::collections::BTreeSet::new();
            for schema in &hosted.descriptor.schemas {
                if crate::exec::is_reserved_schema(&schema.id) {
                    return Err(Refusal::ReservedSchema {
                        world: id,
                        schema: schema.id.as_str().to_string(),
                    });
                }
                let key = (schema.id.as_str().to_string(), schema.version);
                if !seen_versions.insert(key) {
                    return Err(Refusal::DuplicateSchemaVersion {
                        world: id.clone(),
                        schema: schema.id.as_str().to_string(),
                        version: schema.version,
                    });
                }
                // Upgrade claims must reference strictly-older, distinct versions.
                let mut preds = std::collections::BTreeSet::new();
                for &pred in &schema.readable_predecessors {
                    if pred >= schema.version || !preds.insert(pred) {
                        return Err(Refusal::ContradictoryUpgrade {
                            world: id.clone(),
                            schema: schema.id.as_str().to_string(),
                            version: schema.version,
                        });
                    }
                }
            }

            // Declared bounds are policy, so they are checked here rather than
            // in the codec: the descriptor decides canonicality, `build()`
            // decides what a declaration is allowed to say. A declaration may
            // only tighten the substrate's ceiling, never raise it.
            //
            // The list *length* is checked first, and it is not decoration. The
            // descriptor writes each section's entry count as a `u16`, so a
            // World declaring 65,536 entries would encode a count word of zero
            // followed by all of them — an implementation id derived over bytes
            // that `decode` then rejects, which breaks the round trip the whole
            // canonical rule rests on. Refused here rather than made
            // unrepresentable in the codec, because this is where a World's
            // declaration becomes this build's problem.
            if hosted.descriptor.limits.max_payload_bytes == 0
                || hosted.descriptor.limits.max_payload_bytes > crate::world::MAX_PAYLOAD_BYTES
            {
                return Err(Refusal::InvalidLimits(id.clone()));
            }
            for (kind, count) in [
                (Declaration::Scope, hosted.descriptor.scope_schemas.len()),
                (Declaration::Signal, hosted.descriptor.signal_schemas.len()),
            ] {
                if count > MAX_DECLARATIONS_PER_SECTION {
                    return Err(Refusal::InvalidDeclaration {
                        world: id.clone(),
                        kind,
                        name: format!("{count} declarations"),
                    });
                }
            }

            let mut seen_scopes: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for scope in &hosted.descriptor.scope_schemas {
                let name = scope.name.as_str().to_string();
                if !seen_scopes.insert(name.clone()) {
                    return Err(Refusal::DuplicateDeclaration {
                        world: id.clone(),
                        kind: Declaration::Scope,
                        name,
                    });
                }
                if scope.max_key_bytes == 0
                    || scope.max_key_bytes as usize > crate::transient::MAX_SCOPE_FIELD_BYTES
                {
                    return Err(Refusal::InvalidDeclaration {
                        world: id.clone(),
                        kind: Declaration::Scope,
                        name,
                    });
                }
            }

            let mut seen_signals: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for signal in &hosted.descriptor.signal_schemas {
                let name = signal.name.as_str().to_string();
                if !seen_signals.insert(name.clone()) {
                    return Err(Refusal::DuplicateDeclaration {
                        world: id.clone(),
                        kind: Declaration::Signal,
                        name,
                    });
                }
                let bound_ok = signal.max_payload_bytes > 0
                    && signal.max_payload_bytes as usize <= crate::plane::bounds::MAX_SIGNAL_BYTES;
                // The bytes are what policy evaluates, so they are parsed here
                // rather than carried unread into a reviewed identity that
                // would fail the first time anyone sent the signal.
                let demand_ok =
                    mechanics::authorization::AuthorizationDemand::decode_canonical(&signal.demand)
                        .is_ok();
                if !bound_ok || !demand_ok {
                    return Err(Refusal::InvalidDeclaration {
                        world: id.clone(),
                        kind: Declaration::Signal,
                        name,
                    });
                }
            }

            validate_find(&id, &hosted.descriptor)?;
            validate_exec(&id, &hosted.descriptor)?;

            for schema in &hosted.descriptor.schemas {
                let coordinate = (id.clone(), schema.id.clone(), schema.version);
                if body_contracts
                    .insert(coordinate, schema.clone())
                    .is_some_and(|prior| prior != *schema)
                {
                    return Err(Refusal::DuplicateSchemaVersion {
                        world: id.clone(),
                        schema: schema.id.as_str().to_string(),
                        version: schema.version,
                    });
                }
            }

            let implementation = hosted.reviewed_implementation;
            if worlds
                .entry(id.clone())
                .or_default()
                .insert(implementation, Arc::new(hosted))
                .is_some()
            {
                return Err(Refusal::DuplicateWorld(id));
            }
        }
        Ok(Catalog {
            worlds: Arc::new(worlds),
        })
    }
}

fn validate_exec(world: &WorldId, descriptor: &Descriptor) -> Result<(), Refusal> {
    if descriptor.exec_specs.len() > MAX_DECLARATIONS_PER_SECTION {
        return Err(Refusal::TooManyExecSpecs {
            world: world.clone(),
            count: descriptor.exec_specs.len(),
        });
    }
    let mut find_schemas: Vec<_> = descriptor
        .find_schemas
        .iter()
        .map(crate::find::Schema::canonicalized)
        .collect();
    find_schemas.sort_by(|left, right| left.reference.cmp(&right.reference));

    let mut seen = std::collections::BTreeSet::new();
    for spec in &descriptor.exec_specs {
        let reference = exec::SchemaRef {
            name: spec.name.clone(),
            version: spec.version,
        };
        if !seen.insert(reference.clone()) {
            return Err(Refusal::DuplicateExecSpec {
                world: world.clone(),
                spec: reference,
            });
        }
        if spec.validate_with_find(&find_schemas).is_err() {
            return Err(Refusal::InvalidExecSpec {
                world: world.clone(),
                spec: reference,
            });
        }
    }
    Ok(())
}

fn validate_find(world: &WorldId, descriptor: &Descriptor) -> Result<(), Refusal> {
    let body_sources: std::collections::BTreeSet<SourceRef> = descriptor
        .schemas
        .iter()
        .map(|schema| SourceRef {
            name: schema.id.clone(),
            version: schema.version,
        })
        .collect();

    let mut find_refs = std::collections::BTreeSet::new();
    for schema in &descriptor.find_schemas {
        if !find_refs.insert(schema.reference.clone()) {
            return Err(Refusal::DuplicateFindSchema {
                world: world.clone(),
                schema: schema.reference.clone(),
            });
        }
    }

    let mut declared = std::collections::BTreeSet::new();
    for schema in &descriptor.find_schemas {
        let canonical = schema.canonicalized();
        if canonical.validate().is_err()
            || canonical
                .edges
                .iter()
                .any(|edge| !find_refs.contains(&edge.target))
        {
            return Err(Refusal::InvalidFindDeclaration {
                world: world.clone(),
                schema: schema.reference.clone(),
            });
        }
        for source in &canonical.sources {
            if !body_sources.contains(source) {
                return Err(Refusal::MissingFindSource {
                    world: world.clone(),
                    schema: canonical.reference.clone(),
                    source: source.clone(),
                });
            }
            declared.insert((canonical.reference.clone(), source.clone()));
        }
    }

    let mut supplied = std::collections::BTreeSet::new();
    for extractor in &descriptor.find_extractors {
        let coordinate = (extractor.schema.clone(), extractor.source.clone());
        if extractor.abi_version != crate::find::EXTRACTOR_ABI_VERSION
            || extractor.semantic_digest == [0; 32]
            || extractor.shape.validate().is_err()
        {
            return Err(Refusal::InvalidFindDeclaration {
                world: world.clone(),
                schema: extractor.schema.clone(),
            });
        }
        if !supplied.insert(coordinate.clone()) {
            return Err(Refusal::DuplicateFindExtractor {
                world: world.clone(),
                extractor: extractor.clone(),
            });
        }
        if !declared.contains(&coordinate) {
            return Err(Refusal::ExtraFindExtractor {
                world: world.clone(),
                extractor: extractor.clone(),
            });
        }
    }
    if let Some((schema, source)) = declared.difference(&supplied).next() {
        return Err(Refusal::MissingFindExtractor {
            world: world.clone(),
            extractor: Extractor {
                schema: schema.clone(),
                source: source.clone(),
                abi_version: crate::find::EXTRACTOR_ABI_VERSION,
                semantic_digest: [0; 32],
                shape: crate::find::ExtractionShape::new(1, 1, 1, 0, 0, 0),
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Rejection;
    use crate::world::{
        Context, Effect, Intent, Projection, Query, ScopeSchema, SignalSchema, World,
    };
    use replica::body::{EncodingId, SchemaId};
    use replica::body::{MutationModel, Schema};

    /// A minimal test-only World — the conformance harness's stand-in. It stages
    /// nothing and exists only to prove registry behavior.
    struct TestWorld {
        id: WorldId,
        schemas: Vec<Schema>,
        limits: crate::world::Limits,
        scope_schemas: Vec<ScopeSchema>,
        signal_schemas: Vec<SignalSchema>,
        find_schemas: Vec<crate::find::Schema>,
        find_extractors: Vec<crate::find::Extractor>,
        exec_specs: Vec<crate::exec::Spec>,
        hide_find_in_descriptor: bool,
        hide_exec_in_descriptor: bool,
    }

    impl World for TestWorld {
        fn descriptor(&self) -> Descriptor {
            Descriptor {
                id: self.id.clone(),
                implementation_version: crate::world::Version(1),
                schemas: self.schemas.clone(),
                limits: self.limits,
                scope_schemas: self.scope_schemas.clone(),
                signal_schemas: self.signal_schemas.clone(),
                find_schemas: if self.hide_find_in_descriptor {
                    Vec::new()
                } else {
                    self.find_schemas.clone()
                },
                find_extractors: if self.hide_find_in_descriptor {
                    Vec::new()
                } else {
                    self.find_extractors.clone()
                },
                exec_specs: if self.hide_exec_in_descriptor {
                    Vec::new()
                } else {
                    self.exec_specs.clone()
                },
            }
        }
        fn id(&self) -> WorldId {
            self.id.clone()
        }
        fn schemas(&self) -> &[Schema] {
            &self.schemas
        }
        fn scope_schemas(&self) -> &[ScopeSchema] {
            &self.scope_schemas
        }
        fn signal_schemas(&self) -> &[SignalSchema] {
            &self.signal_schemas
        }
        fn find_schemas(&self) -> &[crate::find::Schema] {
            &self.find_schemas
        }
        fn find_extractors(&self) -> &[crate::find::Extractor] {
            &self.find_extractors
        }
        fn exec_specs(&self) -> &[crate::exec::Spec] {
            &self.exec_specs
        }
        fn submit(&self, _ctx: &mut Context<'_>, _intent: Intent) -> Result<Effect, Rejection> {
            Err(Rejection::InvalidRequest)
        }
        fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
            Err(Rejection::InvalidRequest)
        }
    }

    fn schema(id: &str, version: u32, preds: Vec<u32>) -> Schema {
        Schema {
            id: SchemaId::parse(id).unwrap(),
            version,
            encoding: EncodingId::parse("lait.body.v1").unwrap(),
            mutation: MutationModel::Atomic,
            readable_predecessors: preds,
        }
    }

    fn test_world(id: &str, schemas: Vec<Schema>) -> Arc<dyn World> {
        test_world_with_limits(id, schemas, crate::world::Limits::default())
    }

    fn test_world_with_limits(
        id: &str,
        schemas: Vec<Schema>,
        limits: crate::world::Limits,
    ) -> Arc<dyn World> {
        let wid = WorldId::parse(id).unwrap();
        Arc::new(TestWorld {
            id: wid,
            schemas,
            limits,
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: Vec::new(),
            find_extractors: Vec::new(),
            exec_specs: Vec::new(),
            hide_find_in_descriptor: false,
            hide_exec_in_descriptor: false,
        })
    }

    fn find_bound() -> crate::find::Bound {
        crate::find::Bound {
            decoded_bodies: 10,
            postings_read: 10,
            edges_visited: 10,
            nodes_visited: 10,
            paths_retained: 10,
            candidates_per_branch: 10,
            score_evaluations: 10,
            projected_bytes: 10,
            packed_tokens: 10,
            wall_millis: 10,
        }
    }

    fn find_schema(name: &str, source: &str, source_version: u32) -> crate::find::Schema {
        crate::find::Schema {
            reference: crate::find::SchemaRef {
                name: SchemaId::parse(name).unwrap(),
                version: 1,
            },
            sources: vec![SourceRef {
                name: SchemaId::parse(source).unwrap(),
                version: source_version,
            }],
            fields: Vec::new(),
            edges: Vec::new(),
            gates: Vec::new(),
            analyzers: Vec::new(),
            features: Vec::new(),
            ops: crate::find::OpSet::SEEK,
            modes: crate::find::ModeSet::EXACT,
            bound: find_bound(),
        }
    }

    fn demand(capability: &str) -> Vec<u8> {
        mechanics::authorization::AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("com.example.product", capability),
            mechanics::authorization::Resource::root("com.example.product"),
        )
        .encode_canonical()
        .unwrap()
    }

    fn exec_spec(name: &str, query_schema: Option<&str>) -> crate::exec::Spec {
        let payload = |name: &str| crate::exec::PayloadSpec {
            schema: crate::exec::SchemaRef {
                name: SchemaId::parse(name).unwrap(),
                version: 1,
            },
            max_inline_bytes: 1_024,
            max_content_refs: 1,
            max_content_bytes: 4_096,
            read: demand("payload.read"),
            max_additional_input_bytes: 0,
        };
        crate::exec::Spec {
            name: SchemaId::parse(name).unwrap(),
            version: 1,
            access: crate::exec::Access {
                start: demand("exec.start"),
                offer: demand("exec.offer"),
                control: demand("exec.control"),
                accept: demand("exec.accept"),
            },
            input: payload("exec.input"),
            output: payload("exec.output"),
            mode: crate::exec::Mode::Unary,
            resume: crate::exec::Resume::Restart,
            effects: crate::exec::Effects::Pure,
            accept: crate::exec::AcceptRule::World,
            queries: query_schema
                .map(|name| {
                    vec![crate::find::Grant {
                        schemas: vec![crate::find::SchemaRef {
                            name: SchemaId::parse(name).unwrap(),
                            version: 1,
                        }],
                        ops: crate::find::OpSet::SEEK,
                        fields: Vec::new(),
                        edges: Vec::new(),
                        gates: Vec::new(),
                        modes: crate::find::ModeSet::EXACT,
                        features: Vec::new(),
                        bound: find_bound(),
                    }]
                })
                .unwrap_or_default(),
            service: None,
            links: Vec::new(),
            limits: crate::exec::Limits {
                attempts: 2,
                events: 64,
                checkpoints: 0,
                child_runs: 2,
                progress_bytes: 4_096,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
        }
    }

    fn exec_world(
        declarations: Vec<crate::find::Schema>,
        specs: Vec<crate::exec::Spec>,
        hide_exec_in_descriptor: bool,
    ) -> Arc<dyn World> {
        let extractors = declarations
            .iter()
            .flat_map(|declaration| {
                declaration.sources.iter().cloned().map(|source| Extractor {
                    schema: declaration.reference.clone(),
                    source,
                    abi_version: crate::find::EXTRACTOR_ABI_VERSION,
                    semantic_digest: [0x51; 32],
                    shape: crate::find::ExtractionShape::new(1, 8, 8, 4 * 1024, 4 * 1024, 8 * 1024),
                })
            })
            .collect();
        Arc::new(TestWorld {
            id: WorldId::parse("com.example.product").unwrap(),
            schemas: vec![schema("issue", 1, vec![])],
            limits: crate::world::Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: declarations,
            find_extractors: extractors,
            exec_specs: specs,
            hide_find_in_descriptor: false,
            hide_exec_in_descriptor,
        })
    }

    fn find_world(
        declarations: Vec<crate::find::Schema>,
        extractors: Vec<crate::find::Extractor>,
    ) -> Arc<dyn World> {
        Arc::new(TestWorld {
            id: WorldId::parse("com.example.product").unwrap(),
            schemas: vec![schema("issue", 1, vec![])],
            limits: crate::world::Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: declarations,
            find_extractors: extractors,
            exec_specs: Vec::new(),
            hide_find_in_descriptor: false,
            hide_exec_in_descriptor: false,
        })
    }

    fn hidden_find_world(declaration: crate::find::Schema) -> Arc<dyn World> {
        let extractor = Extractor {
            schema: declaration.reference.clone(),
            source: declaration.sources[0].clone(),
            abi_version: crate::find::EXTRACTOR_ABI_VERSION,
            semantic_digest: [0x52; 32],
            shape: crate::find::ExtractionShape::new(1, 8, 8, 4 * 1024, 4 * 1024, 8 * 1024),
        };
        Arc::new(TestWorld {
            id: WorldId::parse("com.example.product").unwrap(),
            schemas: vec![schema("issue", 1, vec![])],
            limits: crate::world::Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: vec![declaration],
            find_extractors: vec![extractor],
            exec_specs: Vec::new(),
            hide_find_in_descriptor: true,
            hide_exec_in_descriptor: false,
        })
    }

    #[test]
    fn single_world_builds_and_is_queryable() {
        let world = test_world("com.example.product", vec![schema("issue", 1, vec![])]);
        let registry = Builder::new().register(world).build().unwrap();
        assert_eq!(registry.len(), 1);
        let id = WorldId::parse("com.example.product").unwrap();
        assert!(registry.contains(&id));
        assert!(registry.world(&id).is_some());
    }

    #[test]
    fn world_payload_limit_must_be_nonzero_and_tighten_runtime() {
        for max_payload_bytes in [0, crate::world::MAX_PAYLOAD_BYTES.saturating_add(1)] {
            let world = test_world_with_limits(
                "com.example.product",
                vec![schema("issue", 1, vec![])],
                crate::world::Limits { max_payload_bytes },
            );
            assert_eq!(
                Builder::new().register(world).build().unwrap_err(),
                Refusal::InvalidLimits(WorldId::parse("com.example.product").unwrap())
            );
        }
    }

    #[test]
    fn reviewed_world_resolves_only_under_its_exact_implementation() {
        let world = test_world("com.example.product", vec![schema("issue", 1, vec![])]);
        let registry = Builder::new()
            .register_reviewed(world, [7; 32])
            .build()
            .unwrap();
        let id = WorldId::parse("com.example.product").unwrap();

        assert!(registry.world_for(&id, [7; 32]).is_some());
        assert!(registry.world_for(&id, [8; 32]).is_none());
    }

    #[test]
    fn multiple_reviewed_implementations_resolve_exactly_without_an_implicit_current() {
        let old = test_world("com.example.product", vec![schema("issue", 1, vec![])]);
        let current = test_world("com.example.product", vec![schema("issue", 1, vec![])]);
        let registry = Builder::new()
            .register_reviewed(old, [7; 32])
            .register_reviewed(current, [8; 32])
            .build()
            .unwrap();
        let id = WorldId::parse("com.example.product").unwrap();

        assert_eq!(registry.len(), 1);
        assert!(registry.world(&id).is_none());
        assert!(registry.descriptor(&id).is_none());
        assert!(registry.world_for(&id, [7; 32]).is_some());
        assert!(registry.world_for(&id, [8; 32]).is_some());
        assert!(registry.world_for(&id, [9; 32]).is_none());
        assert!(registry.descriptor_for(&id, [7; 32]).is_some());
        assert_eq!(registry.descriptors(&id).count(), 2);
    }

    #[test]
    fn duplicate_world_id_is_rejected() {
        let w1 = test_world("com.example.product", vec![schema("issue", 1, vec![])]);
        let w2 = test_world("com.example.product", vec![schema("issue", 1, vec![])]);
        let err = Builder::new()
            .register(w1)
            .register(w2)
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            Refusal::DuplicateWorld(WorldId::parse("com.example.product").unwrap())
        );
    }

    #[test]
    fn duplicate_schema_version_is_rejected() {
        let world = test_world(
            "com.example.product",
            vec![schema("issue", 1, vec![]), schema("issue", 1, vec![])],
        );
        let err = Builder::new().register(world).build().unwrap_err();
        assert!(matches!(err, Refusal::DuplicateSchemaVersion { .. }));
    }

    #[test]
    fn runtime_exec_body_schemas_are_reserved_at_every_version() {
        for (index, reserved) in crate::exec::RESERVED_SCHEMAS.iter().enumerate() {
            let world = test_world(
                "com.example.product",
                vec![schema(reserved, u32::try_from(index + 7).unwrap(), vec![])],
            );
            assert_eq!(
                Builder::new().register(world).build().unwrap_err(),
                Refusal::ReservedSchema {
                    world: WorldId::parse("com.example.product").unwrap(),
                    schema: (*reserved).to_string(),
                }
            );
        }
    }

    #[test]
    fn contradictory_upgrade_claim_is_rejected() {
        // A v1 schema cannot "read" predecessor v1 (not strictly older).
        let world = test_world("com.example.product", vec![schema("issue", 1, vec![1])]);
        let err = Builder::new().register(world).build().unwrap_err();
        assert!(matches!(err, Refusal::ContradictoryUpgrade { .. }));

        // A valid upgrade (v2 reads v1) is accepted.
        let world = test_world(
            "com.example.product",
            vec![schema("issue", 1, vec![]), schema("issue", 2, vec![1])],
        );
        assert!(Builder::new().register(world).build().is_ok());
    }

    #[test]
    fn find_sources_and_extractors_bind_one_to_one() {
        let declaration = find_schema("records", "issue", 1);
        let extractor = Extractor {
            schema: declaration.reference.clone(),
            source: declaration.sources[0].clone(),
            abi_version: crate::find::EXTRACTOR_ABI_VERSION,
            semantic_digest: [0x55; 32],
            shape: crate::find::ExtractionShape::new(1, 8, 8, 4 * 1024, 4 * 1024, 8 * 1024),
        };
        assert!(Builder::new()
            .register(find_world(
                vec![declaration.clone()],
                vec![extractor.clone()],
            ))
            .build()
            .is_ok());

        let missing = Builder::new()
            .register(find_world(vec![declaration.clone()], Vec::new()))
            .build()
            .unwrap_err();
        assert_eq!(
            missing,
            Refusal::MissingFindExtractor {
                world: WorldId::parse("com.example.product").unwrap(),
                extractor: Extractor {
                    semantic_digest: [0; 32],
                    shape: crate::find::ExtractionShape::new(1, 1, 1, 0, 0, 0),
                    ..extractor.clone()
                },
            }
        );

        let extra = Extractor {
            schema: crate::find::SchemaRef {
                name: SchemaId::parse("other").unwrap(),
                version: 1,
            },
            source: extractor.source.clone(),
            abi_version: crate::find::EXTRACTOR_ABI_VERSION,
            semantic_digest: [0x53; 32],
            shape: crate::find::ExtractionShape::new(1, 8, 8, 4 * 1024, 4 * 1024, 8 * 1024),
        };
        assert!(matches!(
            Builder::new()
                .register(find_world(
                    vec![declaration.clone()],
                    vec![extractor, extra]
                ))
                .build(),
            Err(Refusal::ExtraFindExtractor { .. })
        ));

        let duplicated = Extractor {
            schema: declaration.reference.clone(),
            source: declaration.sources[0].clone(),
            abi_version: crate::find::EXTRACTOR_ABI_VERSION,
            semantic_digest: [0x54; 32],
            shape: crate::find::ExtractionShape::new(1, 8, 8, 4 * 1024, 4 * 1024, 8 * 1024),
        };
        assert!(matches!(
            Builder::new()
                .register(find_world(
                    vec![declaration],
                    vec![duplicated.clone(), duplicated],
                ))
                .build(),
            Err(Refusal::DuplicateFindExtractor { .. })
        ));
    }

    #[test]
    fn find_composition_rejects_missing_sources_duplicates_and_cross_wiring() {
        let missing_source = find_schema("records", "comment", 1);
        assert!(matches!(
            Builder::new()
                .register(find_world(vec![missing_source], Vec::new()))
                .build(),
            Err(Refusal::MissingFindSource { .. })
        ));

        let declaration = find_schema("records", "issue", 1);
        assert!(matches!(
            Builder::new()
                .register(find_world(
                    vec![declaration.clone(), declaration.clone()],
                    Vec::new(),
                ))
                .build(),
            Err(Refusal::DuplicateFindSchema { .. })
        ));

        let mut cross_wired = declaration.clone();
        cross_wired.fields.push(crate::find::Field {
            reference: crate::find::FieldRef {
                schema: crate::find::SchemaRef {
                    name: SchemaId::parse("other").unwrap(),
                    version: 1,
                },
                name: SchemaId::parse("title").unwrap(),
            },
            kind: crate::find::FieldKind::Text,
            analyzer: None,
        });
        assert!(matches!(
            Builder::new()
                .register(find_world(vec![cross_wired], Vec::new()))
                .build(),
            Err(Refusal::InvalidFindDeclaration { .. })
        ));

        assert_eq!(
            Builder::new()
                .register(hidden_find_world(find_schema("records", "issue", 1)))
                .build()
                .unwrap_err(),
            Refusal::FindRegistrationMismatch(WorldId::parse("com.example.product").unwrap())
        );
    }

    #[test]
    fn exec_specs_are_composed_with_the_active_find_declaration() {
        let declaration = find_schema("records", "issue", 1);
        let spec = exec_spec("summarize", Some("records"));
        assert!(Builder::new()
            .register(exec_world(
                vec![declaration.clone()],
                vec![spec.clone()],
                false,
            ))
            .build()
            .is_ok());

        let duplicate = Builder::new()
            .register(exec_world(
                vec![declaration.clone()],
                vec![spec.clone(), spec.clone()],
                false,
            ))
            .build()
            .unwrap_err();
        assert_eq!(
            duplicate,
            Refusal::DuplicateExecSpec {
                world: WorldId::parse("com.example.product").unwrap(),
                spec: crate::exec::SchemaRef {
                    name: SchemaId::parse("summarize").unwrap(),
                    version: 1,
                },
            }
        );

        let mut widening = spec.clone();
        widening.queries[0].bound.decoded_bodies += 1;
        assert!(matches!(
            Builder::new()
                .register(exec_world(vec![declaration], vec![widening], false))
                .build(),
            Err(Refusal::InvalidExecSpec { .. })
        ));

        assert_eq!(
            Builder::new()
                .register(exec_world(Vec::new(), vec![spec.clone()], false))
                .build()
                .unwrap_err(),
            Refusal::InvalidExecSpec {
                world: WorldId::parse("com.example.product").unwrap(),
                spec: crate::exec::SchemaRef {
                    name: SchemaId::parse("summarize").unwrap(),
                    version: 1,
                },
            }
        );

        assert_eq!(
            Builder::new()
                .register(exec_world(Vec::new(), vec![spec], true))
                .build()
                .unwrap_err(),
            Refusal::ExecRegistrationMismatch(WorldId::parse("com.example.product").unwrap())
        );
    }
}
