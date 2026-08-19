//! S1 dispatch proof: a test-only World submits and queries through the generic
//! Session dispatch — no product types anywhere. This exercises the
//! envelope → dock → World → Effect/Projection seam the product adopts in S5.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mechanics::ids::{ActorId, DeviceId};

use crate::lifecycle::{Activation, Runtime};
use crate::registry::Builder;
use crate::session::{Conflict, Failure as SessionFailure, ObservationCursor};
use crate::world::Rejection;
use crate::world::{
    AuthorityView, Context, Descriptor, Effect, Intent, Limits, LocalIdentity, PrincipalResolution,
    Projection, Query, Version, World,
};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::body::{MutationModel, Op, Schema};
use replica::frontier::{AuthorityFrontier, ReplicaFrontier};

fn any_demand() -> Vec<u8> {
    mechanics::authorization::AuthorizationDemand::require(
        mechanics::authorization::PolicyCapability::new("w", "c"),
        mechanics::authorization::Resource::root("w"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

/// The writing test device (authorized to mutate by [`SeedAuthority`]).
const WRITER_SEED: [u8; 32] = [41u8; 32];
/// A second device that resolves (docks fine) but is refused every mutation.
const READER_SEED: [u8; 32] = [42u8; 32];

/// A view whose default `authorize_mutation` builds a structurally-valid
/// receipt — the permissive delegate for [`SeedAuthority`]'s allow path.
struct PermissiveAuthority;

impl AuthorityView for PermissiveAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<PrincipalResolution> {
        None
    }
}

/// A test mechanics view: every known-shaped device resolves, but only the
/// writer device passes mutation authorization — the coarse per-device gate
/// lives in the view (as the orbital composition's demand evaluation does),
/// never in the World callback.
struct SeedAuthority;

impl AuthorityView for SeedAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: ActorId::from_incept_hash(&"a".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn authorize_mutation(
        &self,
        space: &mechanics::ids::SpaceId,
        world: &WorldId,
        actor: &ActorId,
        device: &DeviceId,
        authority_frontier: &AuthorityFrontier,
        parent_manifest_root: [u8; 32],
        implementation_id: [u8; 32],
        intent_digest: [u8; 32],
        demand: &[u8],
        operations_digest: [u8; 32],
        core_digest: [u8; 32],
    ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        let writer = mechanics::actor::device_from_seed(&WRITER_SEED);
        if device != &writer {
            return Err(mechanics::authorization::Refusal::Denied(
                mechanics::authorization::DenialReason::DemandUnsatisfied,
            ));
        }
        PermissiveAuthority.authorize_mutation(
            space,
            world,
            actor,
            device,
            authority_frontier,
            parent_manifest_root,
            implementation_id,
            intent_digest,
            demand,
            operations_digest,
            core_digest,
        )
    }
}

fn writer() -> LocalIdentity {
    Runtime::identity_from_seed(&WRITER_SEED)
}

fn reader() -> LocalIdentity {
    Runtime::identity_from_seed(&READER_SEED)
}

fn test_keys() -> Arc<dyn replica::body::BodyKeySource> {
    Arc::new(replica::body::StaticBodyKeys::new(
        mechanics::authorization::AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
    ))
}

/// Sign and submit an intent through the frozen public action API.
fn submit_as(
    session: &crate::session::Session,
    identity: &LocalIdentity,
    intent: Intent,
) -> Result<crate::session::CommittedEffect, SessionFailure> {
    session.submit(identity.sign_action(session, crate::action::RequestId::mint(), intent)?)
}

/// Submit, treating `Busy` as the transient it is documented to be.
///
/// Admission is a `try_lock`: a competing intent gets a prompt typed refusal
/// rather than waiting invisibly behind whatever holds the lane. That makes
/// `Busy` an ordinary answer for any writer that did not happen to win the
/// lane — the product's own router retries on it for exactly this reason.
///
/// A test that wants to observe what a submission does once it is ADMITTED
/// must therefore keep asking. Asking once conflates two different questions,
/// and the answer to the one it did not mean to ask is scheduling: the caller
/// below spawns this immediately after a dock, whose publication work can
/// still hold the lane, so a single attempt was refused often enough to fail
/// this test roughly one run in eight.
fn submit_when_admitted(
    session: &crate::session::Session,
    identity: &LocalIdentity,
    intent: Intent,
) -> Result<crate::session::CommittedEffect, SessionFailure> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match submit_as(session, identity, intent.clone()) {
            Err(SessionFailure::Busy) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            outcome => return outcome,
        }
    }
}

/// A minimal note World: intents carry UTF-8 text; `submit` stages an atomic
/// replacement and reports the touched scope; `query` echoes a deterministic
/// projection derived only from its inputs.
struct NoteWorld {
    id: WorldId,
    schemas: Vec<Schema>,
    find_schemas: Vec<crate::find::Schema>,
    find_extractors: Vec<crate::find::Extractor>,
}

impl NoteWorld {
    fn new() -> Self {
        let find_schema = note_find_schema();
        let find_extractor = crate::find::Extractor {
            schema: find_schema.reference.clone(),
            source: crate::find::SourceRef {
                name: SchemaId::parse("note").unwrap(),
                version: 1,
            },
            abi_version: crate::find::EXTRACTOR_ABI_VERSION,
            semantic_digest: [0x31; 32],
            shape: crate::find::ExtractionShape::new(1, 8, 8, 4 * 1024, 4 * 1024, 8 * 1024),
        };
        Self {
            id: WorldId::parse("com.example.notes").unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("note").unwrap(),
                version: 1,
                encoding: EncodingId::parse("text.utf8").unwrap(),
                mutation: MutationModel::Atomic,
                readable_predecessors: vec![],
            }],
            find_schemas: vec![find_schema],
            find_extractors: vec![find_extractor],
        }
    }
}

impl World for NoteWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn find_schemas(&self) -> &[crate::find::Schema] {
        &self.find_schemas
    }
    fn find_extractors(&self) -> &[crate::find::Extractor] {
        &self.find_extractors
    }
    fn extract(
        &self,
        ctx: &crate::world::ExtractionContext<'_>,
        extractor: &crate::find::Extractor,
        body: &BodyKey,
    ) -> Result<crate::find::BodyExtraction, Rejection> {
        if extractor != &self.find_extractors[0] {
            return Err(Rejection::ContractViolation);
        }
        let value = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        let text = std::str::from_utf8(&value).map_err(|_| Rejection::StateCorrupt)?;
        let schema = self.find_schemas[0].reference.clone();
        let field = self.find_schemas[0].fields[0].reference.clone();
        let terms = text
            .split_whitespace()
            .map(|term| Arc::<[u8]>::from(term.to_lowercase().into_bytes()))
            .collect();
        Ok(crate::find::BodyExtraction {
            body: body.clone(),
            stamp: ctx.body_stamp(body).unwrap_or_default(),
            nodes: vec![crate::find::ExtractedNode {
                key: crate::find::NodeKey {
                    schema,
                    node: crate::find::NodeId::new(b"note".to_vec())
                        .map_err(|_| Rejection::ContractViolation)?,
                },
                gate: None,
                fields: vec![crate::find::ExtractedField {
                    reference: field,
                    value: crate::find::Value::text(text),
                    gate: None,
                    terms,
                }],
                edges: Vec::new(),
                features: Vec::new(),
            }],
        })
    }
    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        if intent.schema.as_str() != "note" {
            return Err(Rejection::UnsupportedSchema);
        }
        // Deterministic body key: same World, a fixed body for this test.
        let key = BodyKey::new(self.id.clone(), BodyId::from_bytes([0u8; 16]));
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            demand: any_demand(),
            operations: vec![(
                key.clone(),
                Op::ReplaceAtomic {
                    value: intent.payload.clone(),
                },
            )],
            bodies: vec![key],
            effect: intent.payload,
            declarations: vec![],
        })
    }
    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        if query.schema.as_str() != "note" {
            return Err(Rejection::UnsupportedSchema);
        }
        // Read the committed Body from the stable snapshot and uppercase it. An
        // absent Body reads as empty.
        let key = BodyKey::new(self.id.clone(), BodyId::from_bytes([0u8; 16]));
        let committed = ctx.read_body(&key)?;
        let text = committed
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(|_| Rejection::InvalidRequest)?
            .unwrap_or_default()
            .to_owned();
        Ok(Projection {
            demand: any_demand(),
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            bytes: text.to_uppercase().into_bytes(),
            frontier: ReplicaFrontier::EMPTY,
            publication: None,
        })
    }
}

fn note_registration() -> (Descriptor, Arc<dyn World>) {
    let world = NoteWorld::new();
    let reg = Descriptor {
        id: world.id(),
        implementation_version: Version(1),
        schemas: world.schemas().to_vec(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: world.find_schemas().to_vec(),
        find_extractors: world.find_extractors().to_vec(),
        exec_specs: Vec::new(),
    };
    (reg, Arc::new(world))
}

struct DescribedWorld {
    descriptor: Descriptor,
    inner: Arc<dyn World>,
}

impl World for DescribedWorld {
    fn descriptor(&self) -> Descriptor {
        self.descriptor.clone()
    }

    fn id(&self) -> WorldId {
        self.inner.id()
    }

    fn schemas(&self) -> &[Schema] {
        self.inner.schemas()
    }

    fn scope_schemas(&self) -> &[crate::world::ScopeSchema] {
        self.inner.scope_schemas()
    }

    fn signal_schemas(&self) -> &[crate::world::SignalSchema] {
        self.inner.signal_schemas()
    }

    fn find_schemas(&self) -> &[crate::find::Schema] {
        self.inner.find_schemas()
    }

    fn find_extractors(&self) -> &[crate::find::Extractor] {
        self.inner.find_extractors()
    }

    fn extract(
        &self,
        ctx: &crate::world::ExtractionContext<'_>,
        extractor: &crate::find::Extractor,
        body: &BodyKey,
    ) -> Result<crate::find::BodyExtraction, Rejection> {
        self.inner.extract(ctx, extractor, body)
    }

    fn exec_specs(&self) -> &[crate::exec::Spec] {
        self.inner.exec_specs()
    }

    fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        self.inner.submit(ctx, intent)
    }

    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        self.inner.query(ctx, query)
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-dispatch-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn find_journal_object(root: &std::path::Path, hash: &[u8; 32]) -> Option<PathBuf> {
    let name = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|value| value.to_str()) == Some(name.as_str()) {
                return Some(path);
            }
        }
    }
    None
}

fn station_with(reg: Descriptor, world: Arc<dyn World>) -> crate::lifecycle::Station {
    let registry = Builder::new()
        .register(Arc::new(DescribedWorld {
            descriptor: reg,
            inner: world,
        }))
        .build()
        .unwrap();
    let rt = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    rt.create().unwrap().open(Activation::default()).unwrap()
}

fn station() -> crate::lifecycle::Station {
    let (reg, world) = note_registration();
    station_with(reg, world)
}

fn exec_demand(capability: &str) -> Vec<u8> {
    mechanics::authorization::AuthorizationDemand::require(
        mechanics::authorization::PolicyCapability::new("com.example.exec-atomic", capability),
        mechanics::authorization::Resource::root("com.example.exec-atomic"),
    )
    .encode_canonical()
    .unwrap()
}

fn exec_schema(name: &str) -> crate::exec::SchemaRef {
    crate::exec::SchemaRef {
        name: SchemaId::parse(name).unwrap(),
        version: 1,
    }
}

fn exec_limits() -> crate::exec::Limits {
    crate::exec::Limits {
        attempts: 1,
        events: 16,
        checkpoints: 0,
        child_runs: 1,
        progress_bytes: 4_096,
        checkpoint_bytes: 0,
        wall_millis: 30_000,
    }
}

fn exec_spec() -> crate::exec::Spec {
    let payload = |name| crate::exec::PayloadSpec {
        schema: exec_schema(name),
        max_inline_bytes: 1_024,
        max_content_refs: 0,
        max_content_bytes: 0,
        read: exec_demand("payload.read"),
        max_additional_input_bytes: 0,
    };
    crate::exec::Spec {
        name: SchemaId::parse("agent.implement").unwrap(),
        version: 1,
        access: crate::exec::Access {
            start: exec_demand("run.start"),
            offer: exec_demand("run.offer"),
            control: exec_demand("run.control"),
            accept: exec_demand("run.accept"),
        },
        input: payload("agent.input"),
        output: payload("agent.output"),
        mode: crate::exec::Mode::Unary,
        resume: crate::exec::Resume::Restart,
        effects: crate::exec::Effects::Pure,
        accept: crate::exec::AcceptRule::World,
        queries: Vec::new(),
        service: None,
        links: Vec::new(),
        limits: exec_limits(),
    }
}

struct ExecAtomicWorld {
    id: WorldId,
    schemas: Vec<Schema>,
    specs: Vec<crate::exec::Spec>,
}

impl ExecAtomicWorld {
    fn new() -> Self {
        Self {
            id: WorldId::parse("com.example.exec-atomic").unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("agent.request").unwrap(),
                version: 1,
                encoding: EncodingId::parse("bytes").unwrap(),
                mutation: MutationModel::Atomic,
                readable_predecessors: Vec::new(),
            }],
            specs: vec![exec_spec()],
        }
    }

    fn product_body(&self) -> BodyKey {
        BodyKey::new(self.id.clone(), BodyId::from_bytes([0x31; 16]))
    }
}

impl World for ExecAtomicWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }

    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    fn exec_specs(&self) -> &[crate::exec::Spec] {
        &self.specs
    }

    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let spec = if intent.payload == b"invalid-spec" {
            exec_schema("agent.missing")
        } else {
            exec_schema("agent.implement")
        };
        Ok(Effect {
            content_refs: Vec::new(),
            exec: vec![crate::exec::Cmd::Start(crate::exec::Start {
                spec,
                build: crate::exec::BuildId::from_bytes([0x32; 32]),
                input: crate::exec::Input {
                    inline: intent.payload.clone(),
                    content: Vec::new(),
                    content_bytes: 0,
                },
                parent: None,
                source: None,
                service: None,
                resources: Vec::new(),
                limits: exec_limits(),
                queries: Vec::new(),
            })],
            operations: vec![(
                self.product_body(),
                Op::ReplaceAtomic {
                    value: intent.payload.clone(),
                },
            )],
            bodies: vec![self.product_body()],
            effect: b"accepted".to_vec(),
            declarations: Vec::new(),
            demand: exec_demand("request.write"),
        })
    }

    fn query(&self, ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        Ok(Projection {
            schema: SchemaId::parse("agent.request").unwrap(),
            schema_version: 1,
            bytes: ctx
                .read_body(&self.product_body())?
                .map(|bytes| bytes.as_ref().to_vec())
                .unwrap_or_default(),
            frontier: ReplicaFrontier::EMPTY,
            demand: exec_demand("request.read"),
            publication: None,
        })
    }
}

fn find_bound(value: u64) -> crate::find::Bound {
    crate::find::Bound {
        decoded_bodies: value,
        postings_read: value,
        edges_visited: value,
        nodes_visited: value,
        paths_retained: value,
        candidates_per_branch: value,
        score_evaluations: value,
        projected_bytes: value,
        packed_tokens: value,
        wall_millis: value,
    }
}

fn note_find_schema() -> crate::find::Schema {
    let schema = crate::find::SchemaRef {
        name: SchemaId::parse("note").unwrap(),
        version: 1,
    };
    let analyzer = crate::find::AnalyzerRef {
        schema: schema.clone(),
        name: SchemaId::parse("token").unwrap(),
    };
    crate::find::Schema {
        reference: schema.clone(),
        sources: vec![crate::find::SourceRef {
            name: SchemaId::parse("note").unwrap(),
            version: 1,
        }],
        fields: vec![crate::find::Field {
            reference: crate::find::FieldRef {
                schema: schema.clone(),
                name: SchemaId::parse("text").unwrap(),
            },
            kind: crate::find::FieldKind::Text,
            analyzer: Some(analyzer.clone()),
        }],
        edges: Vec::new(),
        gates: Vec::new(),
        analyzers: vec![crate::find::Analyzer {
            reference: analyzer,
            configuration: b"test.token".to_vec(),
        }],
        features: Vec::new(),
        ops: crate::find::OpSet::SEEK,
        modes: crate::find::ModeSet::EXACT,
        bound: find_bound(100),
    }
}

fn find_query(value: u64) -> crate::find::Query {
    let schema = crate::find::SchemaRef {
        name: SchemaId::parse("note").unwrap(),
        version: 1,
    };
    crate::find::Query {
        schema: schema.clone(),
        publication: None,
        mode: crate::find::Mode::Exact,
        steps: vec![crate::find::Step {
            id: crate::find::StepId::new(1).unwrap(),
            input: Vec::new(),
            op: crate::find::Op::Seek(crate::find::Seek::Term {
                field: crate::find::FieldRef {
                    schema,
                    name: SchemaId::parse("text").unwrap(),
                },
                text: "q".to_owned(),
                kind: crate::find::Term::Token,
            }),
            bound: find_bound(value),
        }],
        output: crate::find::StepId::new(1).unwrap(),
        bound: find_bound(value),
        page_size: u32::try_from(value)
            .unwrap_or(crate::find::MAX_PAGE_SIZE)
            .clamp(1, crate::find::MAX_PAGE_SIZE),
        cursor: None,
    }
}

#[test]
fn world_mutation_and_started_run_commit_as_one_durable_effect() {
    let world = Arc::new(ExecAtomicWorld::new());
    let world_id = world.id();
    let product_body = world.product_body();
    let station = station_with(world.descriptor(), world);
    let identity = writer();
    let session = station.dock(&world_id, &identity).unwrap();
    let request = crate::action::RequestId::from_bytes([0x44; 16]);
    let payload = b"implement through the issue control seam".to_vec();
    let action = identity
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("agent.request").unwrap(),
                schema_version: 1,
                payload: payload.clone(),
            },
        )
        .unwrap();

    let replay = action.clone();
    let committed = session.submit(action).unwrap();
    let after_first = station.frontier();
    let replayed = session.submit(replay).unwrap();
    assert_eq!(replayed, committed);
    assert_eq!(station.frontier(), after_first);
    let run = crate::exec::derive_run_id(
        station.space_id(),
        &world_id,
        identity.device(),
        request.as_bytes(),
        0,
    );
    let run_body = BodyKey::new(world_id.clone(), BodyId::from_bytes(run.as_bytes()));
    let active_run_body = crate::exec::active_run_body_key(&world_id, run);
    assert_eq!(committed.effect, b"accepted");
    let mut expected_bodies = vec![product_body.clone(), run_body.clone(), active_run_body];
    expected_bodies.sort();
    assert_eq!(committed.bodies, expected_bodies);
    assert_ne!(committed.frontier, ReplicaFrontier::EMPTY);

    let projection = session
        .query(Query {
            schema: SchemaId::parse("agent.request").unwrap(),
            schema_version: 1,
            payload: Vec::new(),
            publication: None,
        })
        .unwrap();
    assert_eq!(projection.bytes, payload);

    let protected = session.test_read_reserved_collaborative(&run_body).unwrap();
    let events = protected.lists.get(crate::exec::RUN_EVENTS_PATH).unwrap();
    assert_eq!(events.len(), 1);
    let event = crate::exec::RunEvent::decode_canonical(&events[0].value).unwrap();
    let started = event.as_started().unwrap();
    assert_eq!(started.run, run);
    assert_eq!(started.request, request.as_bytes());
    assert_eq!(started.command, 0);
    assert_eq!(started.input, exec_schema("agent.input"));
    assert_eq!(
        started.authority_frontier,
        AuthorityFrontier::from_canonical_bytes(vec![1])
    );

    let command = protected.maps.get(crate::exec::RUN_COMMAND_PATH).unwrap();
    let command_bytes = command.values().flatten().copied().collect::<Vec<_>>();
    let decoded = crate::exec::Cmd::decode_canonical(&command_bytes).unwrap();
    assert!(matches!(decoded, crate::exec::Cmd::Start(_)));
    assert_eq!(decoded.digest().unwrap(), started.command_digest);

    let inspected = session
        .work(
            crate::exec::WorkRequest::Inspect {
                world: world_id.clone(),
                run,
            },
            [0x45; 16],
        )
        .unwrap();
    let crate::exec::WorkReply::State(state) = inspected else {
        panic!("an inspect returns lifecycle state");
    };
    assert_eq!(state.run, run);
    assert_eq!(state.event_count, 1);
    assert!(state.unresolved);
    assert!(state.attempts.is_empty());
    let encoded = serde_json::to_string(&state).unwrap();
    assert!(!encoded.contains("input"));
    assert!(!encoded.contains("output"));

    assert!(matches!(
        session
            .work(
                crate::exec::WorkRequest::Watch {
                    world: world_id.clone(),
                    run,
                    known_heads: state.heads.clone(),
                },
                [0x46; 16],
            )
            .unwrap(),
        crate::exec::WorkReply::Unchanged { run: unchanged, .. } if unchanged == run
    ));

    let cancelled = session
        .work(
            crate::exec::WorkRequest::Cancel {
                world: world_id.clone(),
                run,
            },
            [0x47; 16],
        )
        .unwrap();
    let crate::exec::WorkReply::State(cancelled) = cancelled else {
        panic!("a control returns the resulting lifecycle state");
    };
    assert_eq!(cancelled.cancel_asked.len(), 1);
    // CancelAsked is durable intent, not a claim that the executor has
    // stopped. Only a run-level Cancelled fact resolves the Run.
    assert!(cancelled.unresolved);
    assert_eq!(cancelled.event_count, 2);

    assert!(matches!(
        session.work(
            crate::exec::WorkRequest::Retry {
                world: world_id,
                run,
            },
            [0x48; 16],
        ),
        Err(crate::exec::WorkRefusal::Unsupported(_))
    ));
}

#[test]
fn invalid_start_rolls_back_the_companion_world_mutation() {
    let world = Arc::new(ExecAtomicWorld::new());
    let world_id = world.id();
    let station = station_with(world.descriptor(), world);
    let identity = writer();
    let session = station.dock(&world_id, &identity).unwrap();
    let before = station.frontier();

    let result = submit_as(
        &session,
        &identity,
        Intent {
            schema: SchemaId::parse("agent.request").unwrap(),
            schema_version: 1,
            payload: b"invalid-spec".to_vec(),
        },
    );
    assert_eq!(
        result,
        Err(SessionFailure::Rejected(Rejection::ContractViolation))
    );
    assert_eq!(station.frontier(), before);
    let projection = session
        .query(Query {
            schema: SchemaId::parse("agent.request").unwrap(),
            schema_version: 1,
            payload: Vec::new(),
            publication: None,
        })
        .unwrap();
    assert!(projection.bytes.is_empty());
}

#[test]
fn test_world_submits_and_queries_through_dispatch() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();

    // A query before any submit reads the empty committed snapshot.
    let empty = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(empty.bytes, b"");

    // Submit an intent: it is durably committed and advances the frontier.
    let committed = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"hello".to_vec(),
        },
    )
    .unwrap();
    assert_eq!(committed.effect, b"hello");
    assert_eq!(committed.frontier.transaction_count, 1);
    assert_eq!(committed.bodies.len(), 1);
    assert_ne!(committed.frontier, ReplicaFrontier::EMPTY);

    // The query now reads back the committed Body.
    let proj = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(proj.bytes, b"HELLO");
}

#[test]
fn operation_status_is_read_only_durable_and_returns_only_queryable_coordinates() {
    let (_, world) = note_registration();
    let registry = Builder::new().register(world).build().unwrap();
    let runtime = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let space = station.space_id().clone();
    let identity = writer();
    let session = station.dock(&world_id, &identity).unwrap();
    let request = crate::action::RequestId::from_bytes([0x91; 16]);
    let action = identity
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"hello status".to_vec(),
            },
        )
        .unwrap();
    let payload_hash = action.header.payload_hash;

    assert_eq!(
        session
            .operation_status(request.as_bytes(), payload_hash)
            .unwrap(),
        crate::session::OperationStatus::Absent
    );
    assert_eq!(
        session.test_building_memory_bytes(),
        0,
        "an absent cold lookup releases its governor reservation"
    );
    let mut observations = session.observe(None);
    assert!(observations.try_next().unwrap().unwrap().reset);
    let committed = session.submit(action).unwrap();
    let committed_observation = observations.try_next().unwrap().unwrap();
    assert_eq!(
        committed_observation
            .change
            .attribution
            .as_ref()
            .map(|attribution| attribution.operation),
        Some(committed.operation)
    );
    let before_status = station.frontier();
    let found = session
        .operation_status(request.as_bytes(), payload_hash)
        .unwrap();
    let crate::session::OperationStatus::Found {
        receipt,
        publication,
    } = found
    else {
        panic!("the durable operation must be found")
    };
    assert_eq!(receipt.operation, committed.operation);
    assert_eq!(receipt.payload_hash, payload_hash);
    assert_eq!(receipt.effect, committed.effect);
    assert_eq!(receipt.frontier, committed.frontier);
    assert_eq!(receipt.bodies, committed.bodies);
    assert_eq!(receipt.publication, committed.publication.publication);
    assert_eq!(
        publication,
        crate::session::OperationPublication::Ready(committed.publication)
    );
    assert_eq!(station.frontier(), before_status);
    assert!(observations.try_next().unwrap().is_none());
    assert_eq!(
        session.test_building_memory_bytes(),
        0,
        "a successful lookup releases transient decoding memory"
    );
    assert_eq!(
        session.operation_status(request.as_bytes(), [0xff; 32]),
        Err(SessionFailure::Conflict(Conflict::Request))
    );
    assert_eq!(
        session
            .test_receipt_implementation_readiness(request.as_bytes(), payload_hash, [0xab; 32],),
        crate::session::OperationPublication::ImplementationUnavailable
    );
    assert_eq!(
        session.test_building_memory_bytes(),
        0,
        "a conflicting lookup releases transient decoding memory"
    );

    // A crash/reopen evicts the process-local receipt cache and gives the
    // semantic publication a fresh local materialization. The authoritative
    // on-disk point reader still finds the receipt, and `Ready` names that
    // newly installed exact image rather than fabricating the old coordinate.
    drop(observations);
    drop(session);
    drop(station);
    let station = runtime
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world_id, &identity).unwrap();
    let reopened = session
        .operation_status(request.as_bytes(), payload_hash)
        .unwrap();
    assert_eq!(session.test_building_memory_bytes(), 0);
    let crate::session::OperationStatus::Found {
        receipt: reopened_receipt,
        publication: crate::session::OperationPublication::Ready(reopened_publication),
    } = reopened
    else {
        panic!("reopened durable receipt must name a Ready exact publication")
    };
    assert_eq!(
        reopened_receipt.publication,
        committed.publication.publication
    );
    let projection = session
        .query_at(
            reopened_publication,
            Query {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: Vec::new(),
                publication: None,
            },
        )
        .unwrap();
    assert_eq!(projection.bytes, b"HELLO STATUS");
}

#[test]
fn same_request_winner_between_lookup_and_lane_skips_loser_world_callback() {
    struct CountingWorld {
        inner: NoteWorld,
        submits: Arc<AtomicU64>,
    }

    impl World for CountingWorld {
        fn id(&self) -> WorldId {
            self.inner.id()
        }

        fn schemas(&self) -> &[Schema] {
            self.inner.schemas()
        }

        fn find_schemas(&self) -> &[crate::find::Schema] {
            self.inner.find_schemas()
        }

        fn find_extractors(&self) -> &[crate::find::Extractor] {
            self.inner.find_extractors()
        }

        fn extract(
            &self,
            ctx: &crate::world::ExtractionContext<'_>,
            extractor: &crate::find::Extractor,
            body: &BodyKey,
        ) -> Result<crate::find::BodyExtraction, Rejection> {
            self.inner.extract(ctx, extractor, body)
        }

        fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            self.inner.submit(ctx, intent)
        }

        fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
            self.inner.query(ctx, query)
        }
    }

    struct OneShotGateAuthority {
        entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        calls: AtomicU64,
    }

    impl AuthorityView for OneShotGateAuthority {
        fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution> {
            SeedAuthority.resolve(device)
        }

        #[allow(clippy::too_many_arguments)]
        fn authorize_mutation(
            &self,
            space: &mechanics::ids::SpaceId,
            world: &WorldId,
            actor: &ActorId,
            device: &DeviceId,
            authority_frontier: &AuthorityFrontier,
            parent_manifest_root: [u8; 32],
            implementation_id: [u8; 32],
            intent_digest: [u8; 32],
            demand: &[u8],
            operations_digest: [u8; 32],
            core_digest: [u8; 32],
        ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
            SeedAuthority.authorize_mutation(
                space,
                world,
                actor,
                device,
                authority_frontier,
                parent_manifest_root,
                implementation_id,
                intent_digest,
                demand,
                operations_digest,
                core_digest,
            )
        }

        fn evaluate_read(
            &self,
            _actor: &ActorId,
            _authority_frontier: &AuthorityFrontier,
            _demand: &[u8],
        ) -> Result<bool, String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                if let Some(entered) = self.entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let (released, wake) = &*self.release;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            Ok(true)
        }
    }

    let submits = Arc::new(AtomicU64::new(0));
    let mut inner = NoteWorld::new();
    let schema = inner.find_schemas[0].reference.clone();
    inner.find_schemas[0].gates.push(crate::find::Gate {
        reference: crate::find::GateRef {
            schema,
            name: SchemaId::parse("race-gate").unwrap(),
        },
        demand: any_demand(),
    });
    let world = Arc::new(CountingWorld {
        inner,
        submits: submits.clone(),
    });
    let descriptor = Descriptor {
        id: world.id(),
        implementation_version: Version(1),
        schemas: world.schemas().to_vec(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: world.find_schemas().to_vec(),
        find_extractors: world.find_extractors().to_vec(),
        exec_specs: Vec::new(),
    };
    let registry = Builder::new()
        .register(Arc::new(DescribedWorld {
            descriptor,
            inner: world,
        }))
        .build()
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let authority = Arc::new(OneShotGateAuthority {
        entered: std::sync::Mutex::new(Some(entered_tx)),
        release: release.clone(),
        calls: AtomicU64::new(0),
    });
    let runtime = Runtime::open(temp_root(), registry, authority, test_keys());
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = Arc::new(
        station
            .dock(&WorldId::parse("com.example.notes").unwrap(), &writer())
            .unwrap(),
    );
    let request = crate::action::RequestId::from_bytes([0x92; 16]);
    let action = writer()
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"one winner".to_vec(),
            },
        )
        .unwrap();
    let loser_session = session.clone();
    let loser_action = action.clone();
    let loser = std::thread::spawn(move || loser_session.submit(loser_action));
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("loser reached the post-lookup gate");

    let winner = session.submit(action).unwrap();
    let (released, wake) = &*release;
    *released.lock().unwrap() = true;
    wake.notify_all();
    let replayed = loser.join().unwrap().unwrap();
    assert_eq!(replayed, winner);
    assert_eq!(
        submits.load(Ordering::SeqCst),
        1,
        "the post-admission receipt recheck must skip the losing callback"
    );
}

#[test]
fn old_receipt_status_builds_one_exact_retained_publication_off_lock() {
    struct BlockingExtractorWorld {
        inner: NoteWorld,
        blocked: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        started: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
        calls: Arc<AtomicU64>,
    }

    impl World for BlockingExtractorWorld {
        fn id(&self) -> WorldId {
            self.inner.id()
        }
        fn schemas(&self) -> &[Schema] {
            self.inner.schemas()
        }
        fn find_schemas(&self) -> &[crate::find::Schema] {
            self.inner.find_schemas()
        }
        fn find_extractors(&self) -> &[crate::find::Extractor] {
            self.inner.find_extractors()
        }
        fn extract(
            &self,
            ctx: &crate::world::ExtractionContext<'_>,
            extractor: &crate::find::Extractor,
            body: &BodyKey,
        ) -> Result<crate::find::BodyExtraction, Rejection> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (blocked, wake) = &*self.blocked;
            let mut blocked = blocked.lock().unwrap();
            if *blocked {
                if let Some(started) = self.started.lock().unwrap().take() {
                    let _ = started.send(());
                }
                while *blocked {
                    blocked = wake.wait(blocked).unwrap();
                }
            }
            drop(blocked);
            self.inner.extract(ctx, extractor, body)
        }
        fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
            self.inner.submit(ctx, intent)
        }
        fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
            self.inner.query(ctx, query)
        }
    }

    let blocked = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let calls = Arc::new(AtomicU64::new(0));
    let world = Arc::new(BlockingExtractorWorld {
        inner: NoteWorld::new(),
        blocked: blocked.clone(),
        started: std::sync::Mutex::new(Some(started_tx)),
        calls: calls.clone(),
    });
    let world_id = world.id();
    let descriptor = Descriptor {
        id: world_id.clone(),
        implementation_version: Version(1),
        schemas: world.schemas().to_vec(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: world.find_schemas().to_vec(),
        find_extractors: world.find_extractors().to_vec(),
        exec_specs: Vec::new(),
    };
    let station = station_with(descriptor, world);
    let identity = writer();
    let session = station.dock(&world_id, &identity).unwrap();
    let old_request = crate::action::RequestId::from_bytes([0x93; 16]);
    let old_action = identity
        .sign_action(
            &session,
            old_request,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"old receipt".to_vec(),
            },
        )
        .unwrap();
    let old_hash = old_action.header.payload_hash;
    let old = session.submit(old_action).unwrap();
    submit_as(
        &session,
        &identity,
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"current receipt".to_vec(),
        },
    )
    .unwrap();
    session.evict_semantic_publication_for_test(old.publication.publication);
    calls.store(0, Ordering::SeqCst);
    *blocked.0.lock().unwrap() = true;

    let first = session
        .operation_status(old_request.as_bytes(), old_hash)
        .unwrap();
    assert!(matches!(
        first,
        crate::session::OperationStatus::Found {
            publication: crate::session::OperationPublication::Building,
            ..
        }
    ));
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("historical extractor entered the bounded worker");
    // A second caller joins the same semantic publication flight; it never
    // starts another extractor or blocks behind the worker.
    let joined = session
        .operation_status(old_request.as_bytes(), old_hash)
        .unwrap();
    assert!(matches!(
        joined,
        crate::session::OperationStatus::Found {
            publication: crate::session::OperationPublication::Building,
            ..
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let before_read = std::time::Instant::now();
    let current = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: Vec::new(),
            publication: None,
        })
        .unwrap();
    assert_eq!(current.bytes, b"CURRENT RECEIPT");
    assert!(
        before_read.elapsed() < std::time::Duration::from_millis(250),
        "the prior exact view stays responsive while history builds"
    );
    *blocked.0.lock().unwrap() = false;
    blocked.1.notify_all();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let ready = loop {
        let status = session
            .operation_status(old_request.as_bytes(), old_hash)
            .unwrap();
        match status {
            crate::session::OperationStatus::Found {
                publication: crate::session::OperationPublication::Ready(publication),
                ..
            } => break publication,
            crate::session::OperationStatus::Found {
                publication: crate::session::OperationPublication::Building,
                ..
            } if std::time::Instant::now() < deadline => {
                std::thread::yield_now();
            }
            other => panic!("old receipt did not become Ready: {other:?}"),
        }
    };
    assert_eq!(ready.publication, old.publication.publication);
    let projection = session
        .query_at(
            ready,
            Query {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: Vec::new(),
                publication: None,
            },
        )
        .unwrap();
    assert_eq!(projection.bytes, b"OLD RECEIPT");
    assert_eq!(session.test_building_memory_bytes(), 0);
}

#[test]
fn cold_historical_status_reconstructs_the_authenticated_root_after_restart() {
    let root = temp_root();
    let (_, world) = note_registration();
    let registry = Builder::new().register(world).build().unwrap();
    let runtime = Runtime::open(root.clone(), registry, Arc::new(SeedAuthority), test_keys());
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let space = station.space_id().clone();
    let world = WorldId::parse("com.example.notes").unwrap();
    let identity = writer();
    let session = station.dock(&world, &identity).unwrap();
    let request = crate::action::RequestId::from_bytes([0xa1; 16]);
    let action = identity
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"historical after restart".to_vec(),
            },
        )
        .unwrap();
    let payload_hash = action.header.payload_hash;
    let historical = session.submit(action).unwrap();
    submit_as(
        &session,
        &identity,
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"advanced current".to_vec(),
        },
    )
    .unwrap();
    drop(session);
    drop(station);

    let station = runtime
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world, &identity).unwrap();
    let current = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: Vec::new(),
            publication: None,
        })
        .unwrap();
    assert_eq!(current.bytes, b"ADVANCED CURRENT");
    assert!(matches!(
        session
            .operation_status(request.as_bytes(), payload_hash)
            .unwrap(),
        crate::session::OperationStatus::Found {
            publication: crate::session::OperationPublication::Building,
            ..
        }
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let ready = loop {
        match session
            .operation_status(request.as_bytes(), payload_hash)
            .unwrap()
        {
            crate::session::OperationStatus::Found {
                publication: crate::session::OperationPublication::Ready(publication),
                ..
            } => break publication,
            crate::session::OperationStatus::Found {
                publication: crate::session::OperationPublication::Building,
                ..
            } if std::time::Instant::now() < deadline => std::thread::yield_now(),
            other => panic!("cold historical publication did not become Ready: {other:?}"),
        }
    };
    assert_eq!(ready.publication, historical.publication.publication);
    assert_eq!(
        session
            .query_at(
                ready,
                Query {
                    schema: SchemaId::parse("note").unwrap(),
                    schema_version: 1,
                    payload: Vec::new(),
                    publication: None,
                },
            )
            .unwrap()
            .bytes,
        b"HISTORICAL AFTER RESTART"
    );
    assert_eq!(session.test_building_memory_bytes(), 0);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cold_historical_status_refuses_a_long_chain_before_delta_io() {
    let root = temp_root();
    let (_, world) = note_registration();
    let registry = Builder::new().register(world).build().unwrap();
    let runtime = Runtime::open(root.clone(), registry, Arc::new(SeedAuthority), test_keys());
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let space = station.space_id().clone();
    let world = WorldId::parse("com.example.notes").unwrap();
    let identity = writer();
    let session = station.dock(&world, &identity).unwrap();
    for generation in 0u8..63 {
        submit_as(
            &session,
            &identity,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: vec![generation; 8],
            },
        )
        .unwrap();
    }
    let request = crate::action::RequestId::from_bytes([0xa2; 16]);
    let action = identity
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"small old image".to_vec(),
            },
        )
        .unwrap();
    let payload_hash = action.header.payload_hash;
    session.submit(action).unwrap();
    submit_as(
        &session,
        &identity,
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"advanced after long chain".to_vec(),
        },
    )
    .unwrap();
    drop(session);
    drop(station);

    let station = runtime
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world, &identity).unwrap();
    session.constrain_read_cache_to_resident_only_for_test();
    let status = session
        .operation_status(request.as_bytes(), payload_hash)
        .unwrap();
    assert!(
        matches!(
            status,
            crate::session::OperationStatus::Found {
                publication: crate::session::OperationPublication::Capacity,
                ..
            }
        ),
        "unexpected historical status: {status:?}",
    );
    assert_eq!(session.test_building_memory_bytes(), 0);
    assert_eq!(session.read_cache_stats_for_test().0, 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cold_historical_status_maps_generation_index_tamper_without_reconstruction() {
    let root = temp_root();
    let (_, world) = note_registration();
    let registry = Builder::new().register(world).build().unwrap();
    let runtime = Runtime::open(root.clone(), registry, Arc::new(SeedAuthority), test_keys());
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let world = WorldId::parse("com.example.notes").unwrap();
    let identity = writer();
    let session = station.dock(&world, &identity).unwrap();
    let request = crate::action::RequestId::from_bytes([0xa3; 16]);
    let action = identity
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"tampered historical".to_vec(),
            },
        )
        .unwrap();
    let payload_hash = action.header.payload_hash;
    let historical = session.submit(action).unwrap();
    submit_as(
        &session,
        &identity,
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"current survives".to_vec(),
        },
    )
    .unwrap();
    session.evict_generation_for_test(historical.publication.publication.manifest_root);
    let index_root = session
        .generation_index_root_for_test()
        .expect("durable generation index root");
    let object = find_journal_object(&root, &index_root).expect("generation index object");
    std::fs::write(object, b"tampered").unwrap();
    assert!(matches!(
        session
            .operation_status(request.as_bytes(), payload_hash)
            .unwrap(),
        crate::session::OperationStatus::Found {
            publication: crate::session::OperationPublication::GenerationUnavailable,
            ..
        }
    ));
    assert_eq!(session.test_building_memory_bytes(), 0);
    assert_eq!(
        session
            .query(Query {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: Vec::new(),
                publication: None,
            })
            .unwrap()
            .bytes,
        b"CURRENT SURVIVES"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn durable_replay_without_a_ready_read_atom_is_outcome_unknown_not_unaccepted() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let identity = writer();
    let session = station.dock(&world_id, &identity).unwrap();
    let request = crate::action::RequestId::from_bytes([0x94; 16]);
    let action = identity
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"durable but rebuilding".to_vec(),
            },
        )
        .unwrap();
    let replay = action.clone();
    let payload_hash = action.header.payload_hash;
    let committed = session.submit(action).unwrap();
    submit_as(
        &session,
        &identity,
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"new current".to_vec(),
        },
    )
    .unwrap();
    session.mark_semantic_publication_building_for_test(committed.publication.publication);
    let before = station.frontier();

    assert_eq!(session.submit(replay), Err(SessionFailure::OutcomeUnknown));
    assert_eq!(station.frontier(), before);
    assert!(matches!(
        session
            .operation_status(request.as_bytes(), payload_hash)
            .unwrap(),
        crate::session::OperationStatus::Found {
            publication: crate::session::OperationPublication::Building,
            ..
        }
    ));
    session.mark_semantic_publication_capacity_for_test(committed.publication.publication);
    assert!(matches!(
        session
            .operation_status(request.as_bytes(), payload_hash)
            .unwrap(),
        crate::session::OperationStatus::Found {
            publication: crate::session::OperationPublication::Capacity,
            ..
        }
    ));
    assert_eq!(
        session.submit(
            identity
                .sign_action(
                    &session,
                    request,
                    Intent {
                        schema: SchemaId::parse("note").unwrap(),
                        schema_version: 1,
                        payload: b"durable but rebuilding".to_vec(),
                    },
                )
                .unwrap()
        ),
        Err(SessionFailure::ReadCapacity)
    );
}

#[test]
fn authorization_is_checked_per_request() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    // A principal the view refuses to authorize is denied at submit (the
    // mechanics authorizer returns a typed denial), not at dock.
    let session = station.dock(&world_id, &reader()).unwrap();
    let denied = submit_as(
        &session,
        &reader(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"x".to_vec(),
        },
    );
    assert_eq!(
        denied,
        Err(SessionFailure::Rejected(Rejection::Denied(
            crate::world::DeniedCause::DemandUnsatisfied
        )))
    );
}

#[test]
fn many_sessions_dock_independently_without_owning_the_station() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let s1 = station.dock(&world_id, &writer()).unwrap();
    let s2 = station.dock(&world_id, &writer()).unwrap();
    assert_eq!(s1.epoch(), s2.epoch());
    // Undocking one Session leaves the Station and the other Session intact.
    s1.close();
    assert!(s2
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"ok".to_vec(),
            publication: None,
        })
        .is_ok());
    // The Station survives its Sessions and can still go dormant.
    assert!(station.vacate().is_ok());
}

#[test]
fn dormancy_terminates_sessions() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    // Going dormant terminates the Session: further requests fail closed.
    let _orbit = station.vacate().unwrap();
    assert_eq!(
        session.query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"x".to_vec(),
            publication: None,
        }),
        Err(SessionFailure::Interrupted)
    );
}

#[test]
fn a_session_cannot_stop_the_station() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    // Dock a Session and drop it (close) — the Station is unaffected and can
    // still serve new Sessions.
    let s = station.dock(&world_id, &writer()).unwrap();
    s.close();
    let s2 = station.dock(&world_id, &writer()).unwrap();
    assert!(s2
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"ok".to_vec(),
            publication: None,
        })
        .is_ok());
    // A tracked task panicking does not stop the Station's ability to go dormant.
    station.spawn_tracked(|_c| panic!("boom")).unwrap();
    let exit = station.wait();
    assert!(matches!(
        exit.reason,
        Some(crate::lifecycle::ExitReason::TaskFailed)
    ));
}

/// A World whose `submit` panics — to prove Runtime contains it.
struct PanicWorld {
    id: WorldId,
    schemas: Vec<Schema>,
    find_schemas: Vec<crate::find::Schema>,
    find_extractors: Vec<crate::find::Extractor>,
}
impl World for PanicWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn find_schemas(&self) -> &[crate::find::Schema] {
        &self.find_schemas
    }
    fn find_extractors(&self) -> &[crate::find::Extractor] {
        &self.find_extractors
    }
    fn submit(&self, _ctx: &mut Context<'_>, _intent: Intent) -> Result<Effect, Rejection> {
        panic!("world callback panics")
    }
    fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        panic!("generic Find must not enter a World callback")
    }
}

#[test]
fn find_derives_ambient_coordinates_without_entering_the_world() {
    let id = WorldId::parse("com.example.panic").unwrap();
    let schemas = vec![Schema {
        id: SchemaId::parse("note").unwrap(),
        version: 1,
        encoding: EncodingId::parse("text.utf8").unwrap(),
        mutation: MutationModel::Atomic,
        readable_predecessors: vec![],
    }];
    let find_schema = note_find_schema();
    let find_extractor = crate::find::Extractor {
        schema: find_schema.reference.clone(),
        source: crate::find::SourceRef {
            name: SchemaId::parse("note").unwrap(),
            version: 1,
        },
        abi_version: crate::find::EXTRACTOR_ABI_VERSION,
        semantic_digest: [0x32; 32],
        shape: crate::find::ExtractionShape::new(1, 8, 8, 4 * 1024, 4 * 1024, 8 * 1024),
    };
    let reg = Descriptor {
        id: id.clone(),
        implementation_version: Version(1),
        schemas: schemas.clone(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: vec![find_schema.clone()],
        find_extractors: vec![find_extractor.clone()],
        exec_specs: Vec::new(),
    };
    let station = station_with(
        reg,
        Arc::new(PanicWorld {
            id: id.clone(),
            schemas,
            find_schemas: vec![find_schema],
            find_extractors: vec![find_extractor],
        }),
    );
    let session = station.dock(&id, &writer()).unwrap();
    let before = station.frontier();

    let answer = session.find(find_query(1)).unwrap();
    assert!(answer.rows().is_empty());
    assert_eq!(answer.coordinates().world, id);
    assert_eq!(answer.coordinates().root, station.frontier().root);

    let mut resumed = find_query(1);
    let hostile_coordinates = crate::find::Coordinates {
        epoch: mechanics::station::Epoch::from_u64(u64::MAX),
        space: session.space_id().clone(),
        world: id.clone(),
        implementation: [7; 32],
        root: [8; 32],
        extractor_schema_digest: crate::publication::ExtractorSchemaDigest::from_digest([6; 32]),
        materialization: crate::publication::MaterializationId::from_u64(1).unwrap(),
        actor: ActorId::from_incept_hash(&"f".repeat(64)),
        device: mechanics::actor::device_from_seed(&WRITER_SEED),
        authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
        query: resumed.digest().unwrap(),
        schema: resumed.schema.clone(),
    };
    resumed.cursor = Some(
        crate::find::Cursor::issue(&hostile_coordinates, &resumed, b"position".to_vec()).unwrap(),
    );
    assert_eq!(
        session.find(resumed),
        Err(crate::find::Failure::Invalid(
            crate::find::Invalid::CursorMismatch("epoch")
        ))
    );
    assert_eq!(station.frontier(), before, "Find committed nothing");
}

#[test]
fn station_find_policy_can_only_tighten_a_query() {
    let (reg, world) = note_registration();
    let registry = Builder::new()
        .register(Arc::new(DescribedWorld {
            descriptor: reg,
            inner: world,
        }))
        .build()
        .unwrap();
    let rt = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let station = rt
        .create()
        .unwrap()
        .open(Activation {
            find: crate::find::Policy {
                bound: find_bound(1),
            },
            ..Activation::default()
        })
        .unwrap();
    let session = station
        .dock(&WorldId::parse("com.example.notes").unwrap(), &writer())
        .unwrap();

    assert_eq!(
        session.find(find_query(2)),
        Err(crate::find::Failure::PolicyExceeded)
    );
    assert!(session.find(find_query(1)).unwrap().rows().is_empty());
}

/// How long a phase waits for its own worker to reach the gated callback.
///
/// It has to be at least `submit_when_admitted`'s retry budget: a worker that
/// is legitimately still retrying a transient `Busy` has not failed, and a
/// shorter wait here just reports the slower machine as a broken test.
const ENTERED: std::time::Duration = std::time::Duration::from_secs(15);

#[test]
fn historical_find_uses_the_exact_installed_implementation_after_activation_moves() {
    struct SwitchingAuthority {
        implementation: std::sync::Mutex<[u8; 32]>,
    }

    impl AuthorityView for SwitchingAuthority {
        fn resolve(&self, _device: &DeviceId) -> Option<PrincipalResolution> {
            Some(PrincipalResolution {
                actor: ActorId::from_incept_hash(&"a".repeat(64)),
                authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
            })
        }

        fn active_implementation(
            &self,
            _world: &WorldId,
            _authority_frontier: &AuthorityFrontier,
        ) -> Result<Option<[u8; 32]>, String> {
            Ok(Some(*self.implementation.lock().unwrap()))
        }
    }

    struct VersionedNoteWorld {
        inner: NoteWorld,
        extractor: Vec<crate::find::Extractor>,
        marker: &'static str,
        block: Option<Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>>,
        started: Option<std::sync::mpsc::Sender<()>>,
        submit_block: Option<Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>>,
        submit_started: Option<std::sync::mpsc::Sender<()>>,
    }

    impl VersionedNoteWorld {
        fn new(semantic: u8, marker: &'static str) -> Self {
            let inner = NoteWorld::new();
            let mut extractor = inner.find_extractors().to_vec();
            extractor[0].semantic_digest = [semantic; 32];
            Self {
                inner,
                extractor,
                marker,
                block: None,
                started: None,
                submit_block: None,
                submit_started: None,
            }
        }

        fn blocking(
            mut self,
            block: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
            started: std::sync::mpsc::Sender<()>,
        ) -> Self {
            self.block = Some(block);
            self.started = Some(started);
            self
        }

        fn blocking_submit(
            mut self,
            block: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
            started: std::sync::mpsc::Sender<()>,
        ) -> Self {
            self.submit_block = Some(block);
            self.submit_started = Some(started);
            self
        }
    }

    impl World for VersionedNoteWorld {
        fn id(&self) -> WorldId {
            self.inner.id()
        }

        fn schemas(&self) -> &[Schema] {
            self.inner.schemas()
        }

        fn find_schemas(&self) -> &[crate::find::Schema] {
            self.inner.find_schemas()
        }

        fn find_extractors(&self) -> &[crate::find::Extractor] {
            &self.extractor
        }

        fn extract(
            &self,
            ctx: &crate::world::ExtractionContext<'_>,
            extractor: &crate::find::Extractor,
            body: &BodyKey,
        ) -> Result<crate::find::BodyExtraction, Rejection> {
            if extractor != &self.extractor[0] {
                return Err(Rejection::ContractViolation);
            }
            if let Some(started) = &self.started {
                let _ = started.send(());
            }
            if let Some(block) = &self.block {
                let (released, wake) = &**block;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            let value = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
            let text = format!(
                "{} {}",
                self.marker,
                std::str::from_utf8(&value).map_err(|_| Rejection::StateCorrupt)?
            );
            let schema = self.find_schemas()[0].reference.clone();
            let terms = text
                .split_whitespace()
                .map(|term| Arc::<[u8]>::from(term.as_bytes()))
                .collect();
            Ok(crate::find::BodyExtraction {
                body: body.clone(),
                stamp: ctx.body_stamp(body).unwrap_or_default(),
                nodes: vec![crate::find::ExtractedNode {
                    key: crate::find::NodeKey {
                        schema: schema.clone(),
                        node: crate::find::NodeId::new(b"note".to_vec())
                            .map_err(|_| Rejection::ContractViolation)?,
                    },
                    gate: None,
                    fields: vec![crate::find::ExtractedField {
                        reference: self.find_schemas()[0].fields[0].reference.clone(),
                        value: crate::find::Value::text(text),
                        gate: None,
                        terms,
                    }],
                    edges: Vec::new(),
                    features: Vec::new(),
                }],
            })
        }

        fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
            if let Some(started) = &self.submit_started {
                let _ = started.send(());
            }
            if let Some(block) = &self.submit_block {
                let (released, wake) = &**block;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            self.inner.submit(ctx, intent)
        }

        fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
            let mut projection = self.inner.query(ctx, query)?;
            projection.bytes = format!(
                "{}:{}",
                self.marker,
                String::from_utf8(projection.bytes).map_err(|_| Rejection::StateCorrupt)?
            )
            .into_bytes();
            Ok(projection)
        }
    }

    let v1 = [0x71; 32];
    let v2 = [0x72; 32];
    let authority = Arc::new(SwitchingAuthority {
        implementation: std::sync::Mutex::new(v1),
    });
    let block = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let submit_block = Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new()));
    let (submit_started_tx, submit_started_rx) = std::sync::mpsc::channel();
    let old: Arc<dyn World> = Arc::new(VersionedNoteWorld::new(0x41, "old"));
    let current: Arc<dyn World> = Arc::new(
        VersionedNoteWorld::new(0x42, "new")
            .blocking(block.clone(), started_tx)
            .blocking_submit(submit_block.clone(), submit_started_tx),
    );
    let world = old.id();
    let registry = Builder::new()
        .register_reviewed(old, v1)
        .register_reviewed(current, v2)
        .build()
        .unwrap();
    let runtime = Runtime::open(temp_root(), registry, authority.clone(), test_keys());
    let station = Arc::new(
        runtime
            .create()
            .unwrap()
            .open(Activation::default())
            .unwrap(),
    );
    let old_session = station.dock(&world, &writer()).unwrap();
    submit_as(
        &old_session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"body".to_vec(),
        },
    )
    .unwrap();
    let mut old_query = find_query(10);
    if let crate::find::Op::Seek(crate::find::Seek::Term { text, .. }) = &mut old_query.steps[0].op
    {
        *text = "old".to_owned();
    }
    let old_publication = old_session
        .find(old_query.clone())
        .unwrap()
        .coordinates()
        .publication();

    *authority.implementation.lock().unwrap() = v2;
    old_session.publish_authority_advanced();
    let dock_station = station.clone();
    let dock_world = world.clone();
    let dock = std::thread::spawn(move || dock_station.dock(&dock_world, &writer()));
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("new package extractor entered");

    let (frontier_tx, frontier_rx) = std::sync::mpsc::channel();
    let frontier_station = station.clone();
    let frontier = std::thread::spawn(move || {
        let _ = frontier_tx.send(frontier_station.frontier());
    });
    let writer_was_free = frontier_rx
        .recv_timeout(std::time::Duration::from_millis(250))
        .is_ok();
    let (released, wake) = &*block;
    *released.lock().unwrap() = true;
    wake.notify_all();
    frontier.join().unwrap();
    let current_session = Arc::new(dock.join().unwrap().unwrap());
    assert!(
        writer_was_free,
        "current package extraction must not hold the Station writer"
    );

    // Re-arm the same extractor and block a local candidate after the World
    // callback has staged its effect. Exact reads must continue over the prior
    // immutable publication while the one Replica mutation lane serializes
    // the candidate.
    *released.lock().unwrap() = false;
    // Only a token from THIS phase may answer the wait below.
    while started_rx.try_recv().is_ok() {}
    let before_candidate = station.frontier();
    let submit_session = current_session.clone();
    let submit = std::thread::spawn(move || {
        submit_when_admitted(
            &submit_session,
            &writer(),
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"later".to_vec(),
            },
        )
    });
    started_rx
        .recv_timeout(ENTERED)
        .expect("local candidate extractor entered");
    let mut current_query = find_query(10);
    if let crate::find::Op::Seek(crate::find::Seek::Term { text, .. }) =
        &mut current_query.steps[0].op
    {
        *text = "new".to_owned();
    }
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    let read_session = current_session.clone();
    let read = std::thread::spawn(move || {
        let _ = read_tx.send(read_session.find(current_query));
    });
    let prior_read = read_rx.recv_timeout(std::time::Duration::from_millis(250));
    let busy_started = std::time::Instant::now();
    let busy = submit_as(
        &current_session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"competing-extraction".to_vec(),
        },
    );
    assert_eq!(busy, Err(crate::session::Failure::Busy));
    assert!(
        busy_started.elapsed() < std::time::Duration::from_millis(250),
        "competing submit must receive bounded admission feedback"
    );
    let work_busy_started = std::time::Instant::now();
    let work_busy = current_session.work(
        crate::exec::WorkRequest::Cancel {
            world: world.clone(),
            run: crate::exec::RunId::from_bytes([0x33; 16]),
        },
        [0x34; 16],
    );
    assert_eq!(
        work_busy,
        Err(crate::exec::WorkRefusal::Session(
            crate::session::Failure::Busy
        ))
    );
    assert!(
        work_busy_started.elapsed() < std::time::Duration::from_millis(250),
        "mutating Work must receive bounded admission feedback"
    );
    assert_eq!(station.frontier(), before_candidate);
    *released.lock().unwrap() = true;
    wake.notify_all();
    read.join().unwrap();
    let prior_read = prior_read
        .expect("Find must not wait behind local candidate extraction")
        .unwrap();
    assert_eq!(prior_read.rows().len(), 1);
    submit.join().unwrap().unwrap();

    // The domain callback has no Replica or publication-state lock. Runtime
    // does retain the try-admitted operation permit so another writer receives
    // prompt `Busy` rather than waiting invisibly behind this callback.
    let (submit_released, submit_wake) = &*submit_block;
    *submit_released.lock().unwrap() = false;
    // The previous phase's admitted submit ran this same callback with the
    // gate open and left its entry token here. Drain it: otherwise the wait
    // below is answered by that phase, this thread proceeds while the worker
    // is not yet admitted, and its own competing submit wins the lane, enters
    // the gate, and parks on a Condvar only this thread -- now blocked -- was
    // ever going to release. It waits for itself.
    while submit_started_rx.try_recv().is_ok() {}
    let before_callback = station.frontier();
    let callback_session = current_session.clone();
    let callback_submit = std::thread::spawn(move || {
        submit_when_admitted(
            &callback_session,
            &writer(),
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"callback".to_vec(),
            },
        )
    });
    submit_started_rx
        .recv_timeout(ENTERED)
        .expect("World submit callback entered");
    let mut callback_query = find_query(10);
    if let crate::find::Op::Seek(crate::find::Seek::Term { text, .. }) =
        &mut callback_query.steps[0].op
    {
        *text = "new".to_owned();
    }
    let (callback_read_tx, callback_read_rx) = std::sync::mpsc::channel();
    let callback_read_session = current_session.clone();
    let callback_read = std::thread::spawn(move || {
        let _ = callback_read_tx.send(callback_read_session.find(callback_query));
    });
    let callback_prior_read = callback_read_rx
        .recv_timeout(std::time::Duration::from_millis(250))
        .expect("Find must not wait behind the World submit callback")
        .unwrap();
    let callback_busy_started = std::time::Instant::now();
    let callback_busy = submit_as(
        &current_session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"competing-callback".to_vec(),
        },
    );
    assert_eq!(callback_busy, Err(crate::session::Failure::Busy));
    assert!(
        callback_busy_started.elapsed() < std::time::Duration::from_millis(250),
        "slow World callback must not hide a queued writer"
    );
    assert!(!callback_prior_read.rows().is_empty());
    assert_eq!(station.frontier(), before_callback);
    *submit_released.lock().unwrap() = true;
    submit_wake.notify_all();
    callback_read.join().unwrap();
    callback_submit.join().unwrap().unwrap();

    old_query.publication = Some(old_publication);
    let historical = current_session.find(old_query).unwrap();
    assert_eq!(historical.rows().len(), 1);
    assert_eq!(historical.coordinates().implementation, v1);
    assert_eq!(historical.coordinates().publication(), old_publication);

    let historical_projection = current_session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: Vec::new(),
            publication: Some(old_publication),
        })
        .unwrap();
    assert_eq!(historical_projection.bytes, b"old:BODY");
    assert_eq!(
        historical_projection
            .publication
            .expect("exact query publication")
            .publication,
        old_publication
    );
    let current_projection = current_session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: Vec::new(),
            publication: None,
        })
        .unwrap();
    assert_eq!(current_projection.bytes, b"new:CALLBACK");

    let mut current_query = find_query(10);
    if let crate::find::Op::Seek(crate::find::Seek::Term { text, .. }) =
        &mut current_query.steps[0].op
    {
        *text = "new".to_owned();
    }
    let current = current_session.find(current_query).unwrap();
    assert_eq!(current.rows().len(), 1);
    assert_eq!(current.coordinates().implementation, v2);
}

#[test]
fn active_cursor_lease_survives_hot_publication_eviction_then_expires_typed() {
    struct PagedWorld {
        inner: NoteWorld,
    }

    impl World for PagedWorld {
        fn id(&self) -> WorldId {
            self.inner.id()
        }

        fn schemas(&self) -> &[Schema] {
            self.inner.schemas()
        }

        fn find_schemas(&self) -> &[crate::find::Schema] {
            self.inner.find_schemas()
        }

        fn find_extractors(&self) -> &[crate::find::Extractor] {
            self.inner.find_extractors()
        }

        fn extract(
            &self,
            ctx: &crate::world::ExtractionContext<'_>,
            extractor: &crate::find::Extractor,
            body: &BodyKey,
        ) -> Result<crate::find::BodyExtraction, Rejection> {
            if extractor != &self.find_extractors()[0] {
                return Err(Rejection::ContractViolation);
            }
            let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| Rejection::StateCorrupt)?;
            let schema = self.find_schemas()[0].reference.clone();
            let field = self.find_schemas()[0].fields[0].reference.clone();
            let nodes = [b"a".as_slice(), b"b".as_slice()]
                .into_iter()
                .map(|id| crate::find::ExtractedNode {
                    key: crate::find::NodeKey {
                        schema: schema.clone(),
                        node: crate::find::NodeId::new(id.to_vec()).unwrap(),
                    },
                    gate: None,
                    fields: vec![crate::find::ExtractedField {
                        reference: field.clone(),
                        value: crate::find::Value::text(text),
                        gate: None,
                        terms: vec![Arc::from(b"page".as_slice())],
                    }],
                    edges: Vec::new(),
                    features: Vec::new(),
                })
                .collect();
            Ok(crate::find::BodyExtraction {
                body: body.clone(),
                stamp: ctx.body_stamp(body).unwrap_or_default(),
                nodes,
            })
        }

        fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
            self.inner.submit(ctx, intent)
        }

        fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
            self.inner.query(ctx, query)
        }
    }

    let mut inner = NoteWorld::new();
    // This fixture deliberately projects two rows from one source Body; its
    // identity-bound extraction contract must state that rather than relying
    // on the one-row NoteWorld default.
    inner.find_extractors[0].shape =
        crate::find::ExtractionShape::new(2, 8, 16, 4 * 1024, 8 * 1024, 8 * 1024);
    let world: Arc<dyn World> = Arc::new(PagedWorld { inner });
    let id = world.id();
    let registry = Builder::new().register(world).build().unwrap();
    let runtime = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&id, &writer()).unwrap();
    submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"first".to_vec(),
        },
    )
    .unwrap();
    let mut first_query = find_query(10);
    first_query.page_size = 1;
    if let crate::find::Op::Seek(crate::find::Seek::Term { text, .. }) =
        &mut first_query.steps[0].op
    {
        *text = "page".to_owned();
    }
    let first = session.find(first_query.clone()).unwrap();
    assert_eq!(first.rows().len(), 1);
    let cursor = first.next_cursor().cloned().expect("second page cursor");
    // Exceed both hot-generation and hot-publication count caps. The active
    // continuation retains its exact Arc under the separate byte-bounded
    // lease table and does not become an arbitrary current read.
    // Cross the former count-only 256-entry cache boundary. Continuation is
    // protected by its byte-accounted lease, not an implementation constant.
    for index in 0..=256 {
        let result = submit_as(
            &session,
            &writer(),
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: index.to_string().into_bytes(),
            },
        );
        result.unwrap_or_else(|failure| {
            panic!(
                "cursor retention submit {index} failed: {failure:?}; cache={:?}",
                session.read_cache_stats_for_test(),
            )
        });
    }
    let (generations_before_pressure, _, _) = session.read_cache_stats_for_test();
    session.constrain_read_cache_to_authoritative_headroom_for_test();
    submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"pressure".to_vec(),
        },
    )
    .expect("a tiny candidate evicts unpinned acceleration state before reservation");
    let (generations_after, retained_bytes, retained_limit) = session.read_cache_stats_for_test();
    assert!(
        generations_after < generations_before_pressure,
        "an unpinned generation must be reclaimed before admitting the tiny candidate",
    );
    assert!(
        retained_bytes <= retained_limit,
        "the governor must contain the recomputed physical retained set",
    );
    let mut continued = first_query.clone();
    continued.cursor = Some(cursor.clone());
    let second = session.find(continued.clone()).unwrap();
    assert_eq!(second.rows().len(), 1);
    assert_ne!(second.rows()[0].key, first.rows()[0].key);

    session.expire_cursor_leases_for_test();
    assert_eq!(
        session.find(continued),
        Err(crate::find::Failure::PublicationExpired)
    );
}

#[test]
fn exact_world_publication_query_and_find_never_fall_through_materialization() {
    let station = station();
    let world = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world, &writer()).unwrap();
    let schema = SchemaId::parse("note").unwrap();

    submit_as(
        &session,
        &writer(),
        Intent {
            schema: schema.clone(),
            schema_version: 1,
            payload: b"first".to_vec(),
        },
    )
    .unwrap();
    let portable_query = Query {
        schema: schema.clone(),
        schema_version: 1,
        payload: Vec::new(),
        publication: None,
    };
    let first = session.query(portable_query.clone()).unwrap();
    let first_id = first.publication.expect("Runtime stamps an exact WPI");

    submit_as(
        &session,
        &writer(),
        Intent {
            schema: schema.clone(),
            schema_version: 1,
            payload: b"second".to_vec(),
        },
    )
    .unwrap();

    // The prior immutable image remains directly addressable while retained,
    // even though the authority-active publication has moved.
    let first_again = session.query_at(first_id, portable_query.clone()).unwrap();
    assert_eq!(first_again.bytes, b"FIRST");
    assert_eq!(first_again.publication, Some(first_id));

    let mut first_find = find_query(10);
    if let crate::find::Op::Seek(crate::find::Seek::Term { text, .. }) = &mut first_find.steps[0].op
    {
        *text = "first".to_owned();
    }
    let found = session.find_at(first_id, first_find).unwrap();
    assert_eq!(found.coordinates().world_publication(), first_id);
    assert_eq!(found.rows().len(), 1);

    let current = session.query(portable_query.clone()).unwrap();
    let current_id = current.publication.expect("current exact WPI");
    let missing_same_semantic = crate::publication::WorldPublicationId::new(
        current_id.publication,
        current_id.materialization.next(),
    );

    // Portable history still resolves semantically, while the exact overload
    // refuses an unretained materialization rather than silently returning it.
    let mut semantic = portable_query.clone();
    semantic.publication = Some(current_id.publication);
    assert_eq!(session.query(semantic.clone()).unwrap().bytes, b"SECOND");
    assert_eq!(
        session.query_at(missing_same_semantic, semantic),
        Err(SessionFailure::PublicationExpired(missing_same_semantic))
    );

    let mut missing_find = find_query(10);
    if let crate::find::Op::Seek(crate::find::Seek::Term { text, .. }) =
        &mut missing_find.steps[0].op
    {
        *text = "second".to_owned();
    }
    assert_eq!(
        session.find_at(missing_same_semantic, missing_find),
        Err(crate::find::Failure::PublicationExpired)
    );

    let mut mismatched = portable_query;
    mismatched.publication = Some(first_id.publication);
    assert_eq!(
        session.query_at(current_id, mismatched),
        Err(SessionFailure::Rejected(Rejection::ContractViolation))
    );
}

#[test]
fn cold_exact_package_extraction_does_not_hold_the_station_writer() {
    struct FixedImplementationAuthority([u8; 32]);
    impl AuthorityView for FixedImplementationAuthority {
        fn resolve(&self, _device: &DeviceId) -> Option<PrincipalResolution> {
            Some(PrincipalResolution {
                actor: ActorId::from_incept_hash(&"a".repeat(64)),
                authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
            })
        }

        fn active_implementation(
            &self,
            _world: &WorldId,
            _authority_frontier: &AuthorityFrontier,
        ) -> Result<Option<[u8; 32]>, String> {
            Ok(Some(self.0))
        }
    }

    struct BlockingPackage {
        inner: NoteWorld,
        extractor: Vec<crate::find::Extractor>,
        block: Option<Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>>,
        started: Option<std::sync::mpsc::Sender<()>>,
    }

    impl BlockingPackage {
        fn new(
            semantic: u8,
            block: Option<Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>>,
            started: Option<std::sync::mpsc::Sender<()>>,
        ) -> Self {
            let inner = NoteWorld::new();
            let mut extractor = inner.find_extractors().to_vec();
            extractor[0].semantic_digest = [semantic; 32];
            Self {
                inner,
                extractor,
                block,
                started,
            }
        }
    }

    impl World for BlockingPackage {
        fn id(&self) -> WorldId {
            self.inner.id()
        }
        fn schemas(&self) -> &[Schema] {
            self.inner.schemas()
        }
        fn find_schemas(&self) -> &[crate::find::Schema] {
            self.inner.find_schemas()
        }
        fn find_extractors(&self) -> &[crate::find::Extractor] {
            &self.extractor
        }
        fn extract(
            &self,
            ctx: &crate::world::ExtractionContext<'_>,
            extractor: &crate::find::Extractor,
            body: &BodyKey,
        ) -> Result<crate::find::BodyExtraction, Rejection> {
            if extractor != &self.extractor[0] {
                return Err(Rejection::ContractViolation);
            }
            if let Some(started) = &self.started {
                let _ = started.send(());
            }
            if let Some(block) = &self.block {
                let (released, wake) = &**block;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| Rejection::StateCorrupt)?;
            let schema = self.find_schemas()[0].reference.clone();
            Ok(crate::find::BodyExtraction {
                body: body.clone(),
                stamp: ctx.body_stamp(body).unwrap_or_default(),
                nodes: vec![crate::find::ExtractedNode {
                    key: crate::find::NodeKey {
                        schema,
                        node: crate::find::NodeId::new(b"note".to_vec()).unwrap(),
                    },
                    gate: None,
                    fields: vec![crate::find::ExtractedField {
                        reference: self.find_schemas()[0].fields[0].reference.clone(),
                        value: crate::find::Value::text(text),
                        gate: None,
                        terms: vec![Arc::from(text.as_bytes())],
                    }],
                    edges: Vec::new(),
                    features: Vec::new(),
                }],
            })
        }
        fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
            self.inner.submit(ctx, intent)
        }
        fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
            self.inner.query(ctx, query)
        }
    }

    let v1 = [0x81; 32];
    let v2 = [0x82; 32];
    let block = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let old = Arc::new(BlockingPackage::new(
        0x61,
        Some(block.clone()),
        Some(started_tx),
    ));
    let old_digest = crate::publication::ExtractorSchemaDigest::derive(
        old.find_schemas(),
        old.find_extractors(),
    )
    .unwrap();
    let current = Arc::new(BlockingPackage::new(0x62, None, None));
    let world = current.id();
    let registry = Builder::new()
        .register_reviewed(old, v1)
        .register_reviewed(current, v2)
        .build()
        .unwrap();
    let runtime = Runtime::open(
        temp_root(),
        registry,
        Arc::new(FixedImplementationAuthority(v2)),
        test_keys(),
    );
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = Arc::new(station.dock(&world, &writer()).unwrap());
    submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"q".to_vec(),
        },
    )
    .unwrap();
    let old_root = session.snapshot_id().unwrap().root;
    let mut historical = find_query(1);
    historical.publication = Some(crate::publication::PublicationId::new(
        old_root, v1, old_digest,
    ));
    let find_session = session.clone();
    let find = std::thread::spawn(move || find_session.find(historical));
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("historical extractor entered");

    let (committed_tx, committed_rx) = std::sync::mpsc::channel();
    let submit_session = session.clone();
    std::thread::spawn(move || {
        let result = submit_as(
            &submit_session,
            &writer(),
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"later".to_vec(),
            },
        );
        let _ = committed_tx.send(result);
    });
    committed_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("user action must not wait behind historical extraction")
        .unwrap();

    let (released, wake) = &*block;
    *released.lock().unwrap() = true;
    wake.notify_all();
    let answer = find.join().unwrap().unwrap();
    assert_eq!(answer.coordinates().root, old_root);
    assert_eq!(answer.coordinates().implementation, v1);
}

#[test]
fn invalid_station_find_policy_refuses_activation() {
    let (reg, world) = note_registration();
    let registry = Builder::new()
        .register(Arc::new(DescribedWorld {
            descriptor: reg,
            inner: world,
        }))
        .build()
        .unwrap();
    let rt = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let mut invalid = find_bound(1);
    invalid.wall_millis = 0;

    assert!(matches!(
        rt.create().unwrap().open(Activation {
            find: crate::find::Policy { bound: invalid },
            ..Activation::default()
        }),
        Err(crate::lifecycle::Failure::InvalidFindPolicy(_))
    ));
}

#[test]
fn a_world_panic_is_contained_and_does_not_end_the_station() {
    let id = WorldId::parse("com.example.panic").unwrap();
    let schemas = vec![Schema {
        id: SchemaId::parse("note").unwrap(),
        version: 1,
        encoding: EncodingId::parse("text.utf8").unwrap(),
        mutation: MutationModel::Atomic,
        readable_predecessors: vec![],
    }];
    let reg = Descriptor {
        id: id.clone(),
        implementation_version: Version(1),
        schemas: schemas.clone(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: Vec::new(),
        find_extractors: Vec::new(),
        exec_specs: Vec::new(),
    };
    let world: Arc<dyn World> = Arc::new(PanicWorld {
        id: id.clone(),
        schemas,
        find_schemas: Vec::new(),
        find_extractors: Vec::new(),
    });
    let station = station_with(reg, world);
    let session = station.dock(&id, &writer()).unwrap();
    let r = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"x".to_vec(),
        },
    );
    assert_eq!(r, Err(SessionFailure::CallbackPanicked));
    // The Station survives the panic and can still go dormant cleanly.
    assert!(station.vacate().is_ok());
}

#[test]
fn payload_over_the_declared_limit_is_rejected_before_the_callback() {
    let (mut reg, world) = note_registration();
    reg.limits = Limits {
        max_payload_bytes: 4,
    };
    let station = station_with(reg, world);
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let r = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"toolong".to_vec(),
        },
    );
    assert_eq!(r, Err(SessionFailure::Rejected(Rejection::LimitExceeded)));
}

#[test]
fn unregistered_schema_and_version_are_rejected() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    // Unknown schema.
    assert_eq!(
        submit_as(
            &session,
            &writer(),
            Intent {
                schema: SchemaId::parse("other").unwrap(),
                schema_version: 1,
                payload: b"x".to_vec(),
            }
        ),
        Err(SessionFailure::Rejected(Rejection::UnsupportedSchema))
    );
    // Known schema, unknown version.
    assert_eq!(
        submit_as(
            &session,
            &writer(),
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 9,
                payload: b"x".to_vec(),
            }
        ),
        Err(SessionFailure::Rejected(
            Rejection::UnsupportedSchemaVersion
        ))
    );
}

#[test]
fn an_acknowledged_commit_survives_a_crash_without_dormancy() {
    // Finding #1's scenario: submit returns success, then the process dies with
    // NO vacate and NO checkpoint call. Dropping the Station without
    // dormancy models the kill (the OS releases the lock either way). The
    // acknowledged commit must still be there on the next activation, because
    // durability happened AT COMMIT, not at shutdown.
    let (_, world) = note_registration();
    let registry = Builder::new().register(world).build().unwrap();
    let rt = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let world_id = WorldId::parse("com.example.notes").unwrap();

    let orbit = rt.create().unwrap();
    let space = orbit.space_id().clone();
    let station = orbit.open(Activation::default()).unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"ack'd then crash".to_vec(),
        },
    )
    .unwrap();
    // Crash: no dormancy, no checkpoint.
    drop(session);
    drop(station);

    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let proj = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(proj.bytes, b"ACK'D THEN CRASH");
}

#[test]
fn commits_made_during_an_activation_survive_wait_exit() {
    // Finding #1's second scenario: Station::wait returns without a checkpoint.
    // Per-commit durability means nothing made during the activation is lost.
    let (_, world) = note_registration();
    let registry = Builder::new().register(world).build().unwrap();
    let rt = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let world_id = WorldId::parse("com.example.notes").unwrap();

    let station = rt.create().unwrap().open(Activation::default()).unwrap();
    let space = station.space_id().clone();
    let session = station.dock(&world_id, &writer()).unwrap();
    submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"survives wait".to_vec(),
        },
    )
    .unwrap();
    // Exit via wait (no dormancy checkpoint path).
    let exit = station.wait();
    drop(exit);

    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let proj = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(proj.bytes, b"SURVIVES WAIT");
}

#[test]
fn committed_bodies_survive_dormancy_and_reactivation() {
    // The full durable loop: form → activate → submit → vacate (checkpoint)
    // → re-acquire → activate → the committed Body is read back.
    let (_, world) = note_registration();
    let registry = Builder::new().register(world).build().unwrap();
    let rt = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let world_id = WorldId::parse("com.example.notes").unwrap();

    let orbit = rt.create().unwrap();
    let space = orbit.space_id().clone();
    let station = orbit.open(Activation::default()).unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"durable".to_vec(),
        },
    )
    .unwrap();
    // Go dormant: this checkpoints the Replica to the store.
    let orbit = station.vacate().unwrap();
    drop(orbit);

    // Re-acquire and reactivate: the committed Body is restored.
    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let proj = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(proj.bytes, b"DURABLE");
}

/// A hostile World that stages an operation against ANOTHER World's namespace.
struct RogueWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}
impl World for RogueWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        // Attempt to overwrite a Body belonging to com.example.notes.
        let foreign = BodyKey::new(
            WorldId::parse("com.example.notes").unwrap(),
            BodyId::from_bytes([0u8; 16]),
        );
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            demand: any_demand(),
            operations: vec![(
                foreign.clone(),
                Op::ReplaceAtomic {
                    value: intent.payload,
                },
            )],
            bodies: vec![foreign],
            effect: vec![],
            declarations: vec![],
        })
    }
    fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        Err(Rejection::InvalidRequest)
    }
}

#[test]
fn a_world_cannot_write_outside_its_namespace() {
    let id = WorldId::parse("com.example.rogue").unwrap();
    let schemas = vec![Schema {
        id: SchemaId::parse("note").unwrap(),
        version: 1,
        encoding: EncodingId::parse("text.utf8").unwrap(),
        mutation: MutationModel::Atomic,
        readable_predecessors: vec![],
    }];
    let reg = Descriptor {
        id: id.clone(),
        implementation_version: Version(1),
        schemas: schemas.clone(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: Vec::new(),
        find_extractors: Vec::new(),
        exec_specs: Vec::new(),
    };
    let world: Arc<dyn World> = Arc::new(RogueWorld {
        id: id.clone(),
        schemas,
    });
    let station = station_with(reg, world);
    let session = station.dock(&id, &writer()).unwrap();
    let before = station.frontier();
    let r = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"overwrite you".to_vec(),
        },
    );
    assert_eq!(
        r,
        Err(SessionFailure::Rejected(Rejection::ContractViolation))
    );
    // Nothing was committed.
    assert_eq!(station.frontier(), before);
}

/// An authority view whose frontier can be flipped mid-flight — for the CAS test.
struct FlippingAuthority {
    frontier: std::sync::Mutex<Vec<u8>>,
}
impl AuthorityView for FlippingAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: ActorId::from_incept_hash(&"a".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(
                self.frontier.lock().unwrap().clone(),
            ),
        })
    }
}

#[test]
fn a_changed_authority_frontier_refuses_the_commit() {
    // A World that flips the shared authority frontier DURING its callback —
    // modelling a concurrent membership change landing between authorization
    // and commit. The commit-side CAS must catch it.
    struct FlipDuringSubmit {
        inner: NoteWorld,
        authority: Arc<FlippingAuthority>,
    }
    impl World for FlipDuringSubmit {
        fn id(&self) -> WorldId {
            self.inner.id()
        }
        fn schemas(&self) -> &[Schema] {
            self.inner.schemas()
        }
        fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
            *self.authority.frontier.lock().unwrap() = vec![9, 9];
            self.inner.submit(ctx, intent)
        }
        fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
            self.inner.query(ctx, query)
        }
    }

    let authority = Arc::new(FlippingAuthority {
        frontier: std::sync::Mutex::new(vec![1]),
    });
    let inner = NoteWorld::new();
    let id = inner.id();
    let world: Arc<dyn World> = Arc::new(FlipDuringSubmit {
        inner,
        authority: authority.clone(),
    });
    let registry = Builder::new().register(world).build().unwrap();
    let rt = Runtime::open(temp_root(), registry, authority, test_keys());
    let station = rt.create().unwrap().open(Activation::default()).unwrap();
    let session = station.dock(&id, &writer()).unwrap();
    let before = station.frontier();
    let r = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"x".to_vec(),
        },
    );
    assert_eq!(r, Err(SessionFailure::Conflict(Conflict::AuthorityChanged)));
    assert_eq!(station.frontier(), before, "nothing committed");
}

#[test]
fn runtime_stamps_the_projection_frontier() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let committed = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: b"x".to_vec(),
        },
    )
    .unwrap();
    // The NoteWorld returns ReplicaFrontier::EMPTY from query; Runtime must
    // overwrite it with the real committed frontier of the held snapshot.
    let proj = session
        .query(Query {
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(proj.frontier, committed.frontier);
    assert_ne!(proj.frontier, ReplicaFrontier::EMPTY);
}

/// A collaborative World: intents append a comment (list) and bump a counter;
/// queries project the collaborative view.
struct BoardWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}

impl BoardWorld {
    fn new() -> Self {
        Self {
            id: WorldId::parse("com.example.board").unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("card").unwrap(),
                version: 1,
                encoding: EncodingId::parse("collab").unwrap(),
                mutation: MutationModel::Collaborative(
                    replica::body::CollaborativeSchema::default(),
                ),
                readable_predecessors: vec![],
            }],
        }
    }
    fn body(&self) -> BodyKey {
        BodyKey::new(self.id.clone(), BodyId::from_bytes([7u8; 16]))
    }
}

impl World for BoardWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let key = self.body();
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            demand: any_demand(),
            operations: vec![
                (
                    key.clone(),
                    Op::ListInsert {
                        path: "comments".into(),
                        index: ctx
                            .read_collaborative(&key)?
                            .as_deref()
                            .map(|v| v.lists.get("comments").map(|l| l.len()).unwrap_or(0))
                            .unwrap_or(0) as u64,
                        value: intent.payload,
                    },
                ),
                (
                    key.clone(),
                    Op::CounterAdd {
                        path: "activity".into(),
                        delta: 1,
                    },
                ),
            ],
            bodies: vec![key],
            effect: vec![],
            declarations: vec![],
        })
    }
    fn query(&self, ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        let view = ctx
            .read_collaborative(&self.body())?
            .unwrap_or_else(|| crate::world::CollaborativeBody::owned(Default::default()));
        let comments: Vec<String> = view
            .lists
            .get("comments")
            .map(|l| {
                l.iter()
                    .map(|e| String::from_utf8_lossy(&e.value).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let activity = view.counters.get("activity").copied().unwrap_or(0);
        Ok(Projection {
            demand: any_demand(),
            schema: SchemaId::parse("card").unwrap(),
            schema_version: 1,
            bytes: format!("{activity}:{}", comments.join(",")).into_bytes(),
            frontier: ReplicaFrontier::EMPTY,
            publication: None,
        })
    }
}

#[test]
fn a_collaborative_world_commits_and_reads_through_the_session() {
    let world = BoardWorld::new();
    let id = world.id();
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    let rt = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let orbit = rt.create().unwrap();
    let space = orbit.space_id().clone();
    let station = orbit.open(Activation::default()).unwrap();
    let session = station.dock(&id, &writer()).unwrap();

    let query = || Query {
        schema: SchemaId::parse("card").unwrap(),
        schema_version: 1,
        payload: vec![],
        publication: None,
    };
    let intent = |text: &str| Intent {
        schema: SchemaId::parse("card").unwrap(),
        schema_version: 1,
        payload: text.as_bytes().to_vec(),
    };

    submit_as(&session, &writer(), intent("first comment")).unwrap();
    let first_publication = session
        .query(query())
        .unwrap()
        .publication
        .expect("Runtime stamps exact publication")
        .publication;
    submit_as(&session, &writer(), intent("second comment")).unwrap();
    let proj = session.query(query()).unwrap();
    assert_eq!(proj.bytes, b"2:first comment,second comment");
    let mut historical_query = query();
    historical_query.publication = Some(first_publication);
    let historical = session.query(historical_query).unwrap();
    assert_eq!(historical.bytes, b"1:first comment");

    // Collaborative Bodies survive dormancy + reactivation like atomic ones.
    let orbit = station.vacate().unwrap();
    drop(orbit);
    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&id, &writer()).unwrap();
    let proj = session.query(query()).unwrap();
    assert_eq!(proj.bytes, b"2:first comment,second comment");
    let mut historical_query = query();
    historical_query.publication = Some(first_publication);
    let historical = session.query(historical_query).unwrap();
    assert_eq!(historical.bytes, b"1:first comment");
}

struct TextFeedbackWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}

impl TextFeedbackWorld {
    fn new() -> Self {
        Self {
            id: WorldId::parse("com.example.text-feedback").unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("document").unwrap(),
                version: 1,
                encoding: EncodingId::parse("collab").unwrap(),
                mutation: MutationModel::Collaborative(
                    replica::body::CollaborativeSchema::default(),
                ),
                readable_predecessors: Vec::new(),
            }],
        }
    }

    fn body(&self) -> BodyKey {
        BodyKey::new(self.id.clone(), BodyId::from_bytes([0x54; 16]))
    }
}

impl World for TextFeedbackWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }

    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let body = self.body();
        let operations = match intent.payload.as_slice() {
            b"init" => vec![(
                body.clone(),
                Op::TextSplice {
                    path: "text".into(),
                    index: 0,
                    delete: 0,
                    insert: "abcdef".into(),
                },
            )],
            b"edit" => vec![
                (
                    body.clone(),
                    Op::TextSplice {
                        path: "text".into(),
                        index: 1,
                        delete: 2,
                        insert: "X".into(),
                    },
                ),
                (
                    body.clone(),
                    Op::TextSplice {
                        path: "text".into(),
                        index: 2,
                        delete: 1,
                        insert: "YY".into(),
                    },
                ),
            ],
            b"shift" => vec![(
                body.clone(),
                Op::TextSplice {
                    path: "text".into(),
                    index: 0,
                    delete: 0,
                    insert: "Z".into(),
                },
            )],
            b"delete" => vec![(
                body.clone(),
                Op::TextSplice {
                    path: "text".into(),
                    index: 2,
                    delete: 1,
                    insert: String::new(),
                },
            )],
            _ => return Err(Rejection::InvalidRequest),
        };
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            demand: any_demand(),
            operations,
            bodies: vec![body],
            effect: Vec::new(),
            declarations: Vec::new(),
        })
    }

    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        let anchors: Vec<Vec<u8>> =
            postcard::from_bytes(&query.payload).map_err(|_| Rejection::InvalidRequest)?;
        let positions: Vec<Option<u64>> = anchors
            .iter()
            .map(|bytes| {
                let anchor = fabric::Anchor::decode_canonical(bytes)
                    .map_err(|_| Rejection::InvalidRequest)?;
                Ok(match ctx.resolve_anchor(&self.body(), &anchor)? {
                    fabric::AnchorResolution::Resolved(position) => Some(position),
                    fabric::AnchorResolution::Drifted => None,
                })
            })
            .collect::<Result<_, Rejection>>()?;
        Ok(Projection {
            demand: any_demand(),
            schema: SchemaId::parse("document").unwrap(),
            schema_version: 1,
            bytes: postcard::to_stdvec(&positions).unwrap(),
            frontier: ReplicaFrontier::EMPTY,
            publication: None,
        })
    }
}

#[test]
fn prepared_text_feedback_carries_candidate_offsets_and_stable_anchors() {
    let world = Arc::new(TextFeedbackWorld::new());
    let id = world.id();
    let registry = Builder::new().register(world).build().unwrap();
    let runtime = Runtime::open(temp_root(), registry, Arc::new(SeedAuthority), test_keys());
    let station = runtime
        .create()
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&id, &writer()).unwrap();
    let mut observations = session.observe(None);
    assert!(observations.try_next().unwrap().unwrap().reset);
    let intent = |payload: &'static [u8]| Intent {
        schema: SchemaId::parse("document").unwrap(),
        schema_version: 1,
        payload: payload.to_vec(),
    };

    submit_as(&session, &writer(), intent(b"init")).unwrap();
    observations.try_next().unwrap().unwrap();
    let committed = submit_as(&session, &writer(), intent(b"edit")).unwrap();
    let edit_publication = committed.publication.publication;
    let observed = observations.try_next().unwrap().unwrap();
    assert_eq!(observed.frontier, committed.frontier);
    assert_eq!(
        observed.change.attribution.as_ref().map(|a| a.operation),
        Some(committed.operation),
        "the durable acknowledgement and live feedback must name one operation",
    );
    assert_eq!(
        observed.publications,
        vec![crate::session::AffectedWorldPublication {
            world: id.clone(),
            publication: committed.publication,
        }],
        "the live terminal must name the exact acknowledged publication",
    );
    let crate::change::Detail::Exact(changes) = &observed.change.bodies[0].detail else {
        panic!("prepared local text feedback must be exact")
    };
    let ranges: Vec<_> = changes
        .iter()
        .map(|change| change.text.as_ref().unwrap())
        .collect();
    assert_eq!((ranges[0].start, ranges[0].end), (1, 2));
    assert_eq!((ranges[1].start, ranges[1].end), (2, 4));

    let anchors: Vec<Vec<u8>> = ranges
        .iter()
        .flat_map(|range| [range.start_anchor.clone(), range.end_anchor.clone()])
        .collect();
    let resolve = |anchors: &[Vec<u8>], publication| -> Vec<Option<u64>> {
        let projection = session
            .query(Query {
                schema: SchemaId::parse("document").unwrap(),
                schema_version: 1,
                payload: postcard::to_stdvec(anchors).unwrap(),
                publication,
            })
            .unwrap();
        postcard::from_bytes(&projection.bytes).unwrap()
    };
    assert_eq!(
        resolve(&anchors, None),
        vec![Some(1), Some(2), Some(2), Some(4)]
    );

    // A later insertion shifts every retained anchor through the convergent
    // history; the candidate offsets remain the exact coordinates of their
    // stamped Observation and are intentionally not rewritten.
    submit_as(&session, &writer(), intent(b"shift")).unwrap();
    assert_eq!(
        resolve(&anchors, None),
        vec![Some(2), Some(3), Some(3), Some(5)]
    );
    assert_eq!(
        resolve(&anchors, Some(edit_publication)),
        vec![Some(1), Some(2), Some(2), Some(4)],
        "historical anchor resolution stays pinned to its exact publication",
    );

    // Deleting anchored material may collapse or drift the old range, but it
    // must never resolve to an unrelated plausible position. Fabric's total
    // resolution contract reports the honest post-delete state.
    submit_as(&session, &writer(), intent(b"delete")).unwrap();
    let deleted = resolve(&anchors, None);
    assert!(deleted[0].is_none() || deleted[0] == Some(2));
    assert!(deleted[1].is_none() || deleted[1] == Some(2));
}

/// A World registering BOTH mutation models but staging collaborative ops from
/// its ATOMIC schema's intent — the schema-containment violation of finding #8.
struct MixedWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}
impl World for MixedWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn submit(&self, _ctx: &mut Context<'_>, _intent: Intent) -> Result<Effect, Rejection> {
        // Regardless of which schema the intent named, stage a collaborative op.
        let key = BodyKey::new(self.id.clone(), BodyId::from_bytes([5u8; 16]));
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            demand: any_demand(),
            operations: vec![(
                key.clone(),
                Op::CounterAdd {
                    path: "sneak".into(),
                    delta: 1,
                },
            )],
            bodies: vec![key],
            effect: vec![],
            declarations: vec![],
        })
    }
    fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        Err(Rejection::InvalidRequest)
    }
}

#[test]
fn containment_is_bound_to_the_intent_schema_not_the_world() {
    let id = WorldId::parse("com.example.mixed").unwrap();
    let schemas = vec![
        Schema {
            id: SchemaId::parse("atomicdoc").unwrap(),
            version: 1,
            encoding: EncodingId::parse("bytes").unwrap(),
            mutation: MutationModel::Atomic,
            readable_predecessors: vec![],
        },
        Schema {
            id: SchemaId::parse("collabdoc").unwrap(),
            version: 1,
            encoding: EncodingId::parse("collab").unwrap(),
            mutation: MutationModel::Collaborative(replica::body::CollaborativeSchema::default()),
            readable_predecessors: vec![],
        },
    ];
    let reg = Descriptor {
        id: id.clone(),
        implementation_version: Version(1),
        schemas: schemas.clone(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: Vec::new(),
        find_extractors: Vec::new(),
        exec_specs: Vec::new(),
    };
    let world: Arc<dyn World> = Arc::new(MixedWorld {
        id: id.clone(),
        schemas,
    });
    let station = station_with(reg, world);
    let session = station.dock(&id, &writer()).unwrap();
    let before = station.frontier();
    // An ATOMIC-schema intent staging a collaborative op is refused even though
    // the World also registers a collaborative schema.
    let r = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("atomicdoc").unwrap(),
            schema_version: 1,
            payload: vec![],
        },
    );
    assert_eq!(
        r,
        Err(SessionFailure::Rejected(Rejection::ContractViolation))
    );
    assert_eq!(station.frontier(), before, "nothing committed");
    // The SAME staged ops under the collaborative schema are legal.
    submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("collabdoc").unwrap(),
            schema_version: 1,
            payload: vec![],
        },
    )
    .unwrap();
}

#[test]
fn an_identical_signed_replay_returns_the_original_result_without_reapplying() {
    // A collaborative CounterAdd is the canonical non-idempotent operation:
    // replaying the SAME signed action must return the original committed
    // result and must NOT bump the counter again.
    let world = BoardWorld::new();
    let id = world.id();
    let reg = Descriptor {
        id: id.clone(),
        implementation_version: Version(1),
        schemas: world.schemas().to_vec(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
        find_schemas: Vec::new(),
        find_extractors: Vec::new(),
        exec_specs: Vec::new(),
    };
    let station = station_with(reg, Arc::new(world));
    let session = station.dock(&id, &writer()).unwrap();
    let action = writer()
        .sign_action(
            &session,
            crate::action::RequestId::mint(),
            Intent {
                schema: SchemaId::parse("card").unwrap(),
                schema_version: 1,
                payload: b"a comment".to_vec(),
            },
        )
        .unwrap();

    let first = session.submit(action.clone()).unwrap();
    let replay = session.submit(action.clone()).unwrap();
    assert_eq!(
        first, replay,
        "identical replay returns the identical result"
    );
    assert_eq!(
        station.frontier(),
        first.frontier,
        "the replay committed nothing"
    );
    let proj = session
        .query(Query {
            schema: SchemaId::parse("card").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(
        proj.bytes, b"1:a comment",
        "the counter bumped exactly once and the comment inserted exactly once"
    );
}

#[test]
fn reusing_a_request_id_with_a_different_payload_is_a_typed_conflict() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let request = crate::action::RequestId::from_bytes([77u8; 16]);
    let intent = |text: &[u8]| Intent {
        schema: SchemaId::parse("note").unwrap(),
        schema_version: 1,
        payload: text.to_vec(),
    };
    session
        .submit(
            writer()
                .sign_action(&session, request, intent(b"first"))
                .unwrap(),
        )
        .unwrap();
    let after_first = station.frontier();
    let err = session
        .submit(
            writer()
                .sign_action(&session, request, intent(b"second"))
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(err, SessionFailure::Conflict(Conflict::Request));
    assert_eq!(station.frontier(), after_first, "nothing committed");
}

#[test]
fn an_action_for_another_space_or_world_is_refused() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let good = writer()
        .sign_action(
            &session,
            crate::action::RequestId::mint(),
            Intent {
                schema: SchemaId::parse("note").unwrap(),
                schema_version: 1,
                payload: b"x".to_vec(),
            },
        )
        .unwrap();
    // Rebind the header to a different Space: the (re-signed) action no longer
    // addresses this Session and is refused before the World runs.
    let mut header = good.header.clone();
    header.space = mechanics::ids::SpaceId::from_digest([0xAB; 16]);
    let foreign =
        crate::action::SignedWorldAction::sign(header, good.payload.clone(), &WRITER_SEED);
    assert_eq!(
        session.submit(foreign),
        Err(SessionFailure::Rejected(Rejection::InvalidRequest))
    );
    // A tampered (unsigned) mutation of the same header fails signature
    // verification outright.
    let mut tampered = good.clone();
    tampered.header.world = WorldId::parse("com.example.other").unwrap();
    assert_eq!(
        session.submit(tampered),
        Err(SessionFailure::Rejected(Rejection::InvalidRequest))
    );
}

#[test]
fn observation_cursor_starts_at_a_reset_boundary() {
    let station = station();
    let world_id = WorldId::parse("com.example.notes").unwrap();
    let session = station.dock(&world_id, &writer()).unwrap();
    let cursor = ObservationCursor::start(session.epoch());
    assert_eq!(cursor.sequence, 0);
    // First use rebaselines: exactly one reset record.
    let mut stream = session.observe(None);
    let first = stream.try_next().unwrap().unwrap();
    assert!(first.reset);
}
