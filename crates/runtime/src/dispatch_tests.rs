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

/// A minimal note World: intents carry UTF-8 text; `submit` stages an atomic
/// replacement and reports the touched scope; `query` echoes a deterministic
/// projection derived only from its inputs.
struct NoteWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}

impl NoteWorld {
    fn new() -> Self {
        Self {
            id: WorldId::parse("com.example.notes").unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("note").unwrap(),
                version: 1,
                encoding: EncodingId::parse("text.utf8").unwrap(),
                mutation: MutationModel::Atomic,
                readable_predecessors: vec![],
            }],
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
        let committed = ctx.read_body(&key).unwrap_or_default();
        let text = String::from_utf8(committed).map_err(|_| Rejection::InvalidRequest)?;
        Ok(Projection {
            demand: any_demand(),
            schema: SchemaId::parse("note").unwrap(),
            schema_version: 1,
            bytes: text.to_uppercase().into_bytes(),
            frontier: ReplicaFrontier::EMPTY,
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
        find_schemas: Vec::new(),
        find_extractors: Vec::new(),
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
            bytes: ctx.read_body(&self.product_body()).unwrap_or_default(),
            frontier: ReplicaFrontier::EMPTY,
            demand: exec_demand("request.read"),
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

fn find_query(value: u64) -> crate::find::Query {
    let schema = crate::find::SchemaRef {
        name: SchemaId::parse("note").unwrap(),
        version: 1,
    };
    crate::find::Query {
        schema: schema.clone(),
        root: None,
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
    assert_eq!(committed.effect, b"accepted");
    let mut expected_bodies = vec![product_body.clone(), run_body.clone()];
    expected_bodies.sort();
    assert_eq!(committed.bodies, expected_bodies);
    assert_ne!(committed.frontier, ReplicaFrontier::EMPTY);

    let projection = session
        .query(Query {
            schema: SchemaId::parse("agent.request").unwrap(),
            schema_version: 1,
            payload: Vec::new(),
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
        })
        .unwrap();
    assert_eq!(proj.bytes, b"HELLO");
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
}
impl World for PanicWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
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
    let station = station_with(
        reg,
        Arc::new(PanicWorld {
            id: id.clone(),
            schemas,
        }),
    );
    let session = station.dock(&id, &writer()).unwrap();
    let before = station.frontier();

    assert_eq!(
        session.find(find_query(1)),
        Err(crate::find::Failure::Unavailable)
    );

    let mut resumed = find_query(1);
    let hostile_coordinates = crate::find::Coordinates {
        epoch: mechanics::station::Epoch::from_u64(u64::MAX),
        space: session.space_id().clone(),
        world: id.clone(),
        implementation: [7; 32],
        root: [8; 32],
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
    assert_eq!(
        session.find(find_query(1)),
        Err(crate::find::Failure::Unavailable)
    );
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
                            .read_collaborative(&key)
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
        let view = ctx.read_collaborative(&self.body()).unwrap_or_default();
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
    };
    let intent = |text: &str| Intent {
        schema: SchemaId::parse("card").unwrap(),
        schema_version: 1,
        payload: text.as_bytes().to_vec(),
    };

    submit_as(&session, &writer(), intent("first comment")).unwrap();
    let first_generation = session.snapshot_id().unwrap();
    submit_as(&session, &writer(), intent("second comment")).unwrap();
    let proj = session.query(query()).unwrap();
    assert_eq!(proj.bytes, b"2:first comment,second comment");
    let historical = session.query_at(&first_generation, query()).unwrap();
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
    let historical = session.query_at(&first_generation, query()).unwrap();
    assert_eq!(historical.bytes, b"1:first comment");
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
