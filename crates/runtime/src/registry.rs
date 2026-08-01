//! The immutable World registry.
//!
//! World implementations register with a [`RuntimeBuilder`]. `build()` validates
//! and **freezes** the set: descriptors are immutable per Runtime, and dynamic
//! loading is deferred. Runtime rejects duplicate ids, duplicate schema
//! versions within a World, invalid limits, contradictory upgrade claims, and
//! scope/signal declarations that repeat a name, bound it at zero, exceed the
//! substrate ceiling they may only tighten, or carry demand bytes that are not
//! canonical.

use std::collections::BTreeMap;
use std::sync::Arc;

use replica::ids::WorldId;

use crate::world::{Descriptor, World};

/// Which declaration list a registration failure is about.
///
/// One discriminated pair of variants rather than four near-identical ones:
/// the two lists fail for the same reasons and a caller that wants to report
/// them differently already has the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    Scope,
    Signal,
}

/// Why registration was rejected at `build()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Two Worlds registered the same [`WorldId`].
    DuplicateWorld(WorldId),
    /// A World declared the same `(schema id, version)` twice.
    DuplicateSchemaVersion {
        world: WorldId,
        schema: String,
        version: u32,
    },
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
        kind: DeclarationKind,
        name: String,
    },
    /// A declaration's bound is zero, exceeds the substrate ceiling it may only
    /// tighten, or its demand is not canonical.
    InvalidDeclaration {
        world: WorldId,
        kind: DeclarationKind,
        name: String,
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
}

/// The frozen, immutable set of hosted Worlds. Lookup is by [`WorldId`].
#[derive(Clone)]
pub struct Registry {
    worlds: Arc<BTreeMap<WorldId, Arc<Hosted>>>,
}

// `Hosted` holds `Arc<dyn World>`, which is not `Debug`; the registry only ever
// shows which Worlds it hosts, so `Debug` lists the ids rather than deriving.
impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("worlds", &self.worlds.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Registry {
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
        self.worlds.get(id).map(|h| h.world.clone())
    }

    /// The reviewed descriptor for a hosted World, if any.
    pub fn descriptor(&self, id: &WorldId) -> Option<&Descriptor> {
        self.worlds.get(id).map(|h| &h.descriptor)
    }

    /// The hosted World ids, in canonical order.
    pub fn ids(&self) -> impl Iterator<Item = &WorldId> {
        self.worlds.keys()
    }
}

/// Accumulates World registrations, then validates and freezes them into an
/// immutable [`Registry`]. Consumed by `build()`.
#[derive(Default)]
pub struct RuntimeBuilder {
    pending: Vec<Hosted>,
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a World. Its descriptor is obtained from the implementation;
    /// validation is deferred to [`RuntimeBuilder::build`], so ordering never
    /// masks a duplicate.
    pub fn register(mut self, world: Arc<dyn World>) -> Self {
        let descriptor = world.descriptor();
        self.pending.push(Hosted { descriptor, world });
        self
    }

    /// Validate and freeze the registry. Rejects duplicate Worlds/schema
    /// versions, registration/impl mismatch, invalid limits, contradictory
    /// upgrade claims, and invalid or duplicated scope/signal declarations.
    pub fn build(self) -> Result<Registry, Refusal> {
        let mut worlds: BTreeMap<WorldId, Arc<Hosted>> = BTreeMap::new();
        for hosted in self.pending {
            let id = hosted.descriptor.id.clone();

            // Invalid limits (reserved shape; only the "max is expressible"
            // check applies until S1 freezes the bounds).
            // (No invalid limit is currently expressible; the branch stays for
            // when S1 adds real bounds.)

            // Per-World schema validation.
            let mut seen_versions: std::collections::BTreeSet<(String, u32)> =
                std::collections::BTreeSet::new();
            for schema in &hosted.descriptor.schemas {
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
            for (kind, count) in [
                (
                    DeclarationKind::Scope,
                    hosted.descriptor.scope_schemas.len(),
                ),
                (
                    DeclarationKind::Signal,
                    hosted.descriptor.signal_schemas.len(),
                ),
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
                        kind: DeclarationKind::Scope,
                        name,
                    });
                }
                if scope.max_key_bytes == 0
                    || scope.max_key_bytes as usize > crate::transient::MAX_SCOPE_FIELD_BYTES
                {
                    return Err(Refusal::InvalidDeclaration {
                        world: id.clone(),
                        kind: DeclarationKind::Scope,
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
                        kind: DeclarationKind::Signal,
                        name,
                    });
                }
                let bound_ok = signal.max_payload_bytes > 0
                    && signal.max_payload_bytes as usize <= crate::plane::bounds::MAX_SIGNAL_BYTES;
                // The bytes are what policy evaluates, so they are parsed here
                // rather than carried unread into a reviewed identity that
                // would fail the first time anyone sent the signal.
                let demand_ok =
                    mechanics::demand::AuthorizationDemand::decode_canonical(&signal.demand)
                        .is_ok();
                if !bound_ok || !demand_ok {
                    return Err(Refusal::InvalidDeclaration {
                        world: id.clone(),
                        kind: DeclarationKind::Signal,
                        name,
                    });
                }
            }

            if worlds.insert(id.clone(), Arc::new(hosted)).is_some() {
                return Err(Refusal::DuplicateWorld(id));
            }
        }
        Ok(Registry {
            worlds: Arc::new(worlds),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Rejection;
    use crate::world::{
        Context, Effect, Intent, Projection, Query, ScopeSchema, SignalSchema, World,
    };
    use replica::body::{MutationModel, Schema};
    use replica::ids::{EncodingId, SchemaId};

    /// A minimal test-only World — the conformance harness's stand-in. It stages
    /// nothing and exists only to prove registry behavior.
    struct TestWorld {
        id: WorldId,
        schemas: Vec<Schema>,
        scope_schemas: Vec<ScopeSchema>,
        signal_schemas: Vec<SignalSchema>,
    }

    impl World for TestWorld {
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
        let wid = WorldId::parse(id).unwrap();
        Arc::new(TestWorld {
            id: wid,
            schemas,
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
        })
    }

    #[test]
    fn single_world_builds_and_is_queryable() {
        let world = test_world("com.example.issues", vec![schema("issue", 1, vec![])]);
        let registry = RuntimeBuilder::new().register(world).build().unwrap();
        assert_eq!(registry.len(), 1);
        let id = WorldId::parse("com.example.issues").unwrap();
        assert!(registry.contains(&id));
        assert!(registry.world(&id).is_some());
    }

    #[test]
    fn duplicate_world_id_is_rejected() {
        let w1 = test_world("com.example.issues", vec![schema("issue", 1, vec![])]);
        let w2 = test_world("com.example.issues", vec![schema("issue", 1, vec![])]);
        let err = RuntimeBuilder::new()
            .register(w1)
            .register(w2)
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            Refusal::DuplicateWorld(WorldId::parse("com.example.issues").unwrap())
        );
    }

    #[test]
    fn duplicate_schema_version_is_rejected() {
        let world = test_world(
            "com.example.issues",
            vec![schema("issue", 1, vec![]), schema("issue", 1, vec![])],
        );
        let err = RuntimeBuilder::new().register(world).build().unwrap_err();
        assert!(matches!(err, Refusal::DuplicateSchemaVersion { .. }));
    }

    #[test]
    fn contradictory_upgrade_claim_is_rejected() {
        // A v1 schema cannot "read" predecessor v1 (not strictly older).
        let world = test_world("com.example.issues", vec![schema("issue", 1, vec![1])]);
        let err = RuntimeBuilder::new().register(world).build().unwrap_err();
        assert!(matches!(err, Refusal::ContradictoryUpgrade { .. }));

        // A valid upgrade (v2 reads v1) is accepted.
        let world = test_world(
            "com.example.issues",
            vec![schema("issue", 1, vec![]), schema("issue", 2, vec![1])],
        );
        assert!(RuntimeBuilder::new().register(world).build().is_ok());
    }
}
