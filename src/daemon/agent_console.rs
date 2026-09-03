//! Identity-local Console supervisor.
//!
//! Correspondence selects an owner-authored operation; a private, unregistered
//! Space supplies Runtime's durable Run truth. The agent signs the World action
//! and performs it as itself under recorded owner delegation. No command is
//! ever handed directly to the OCI backend.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent::{
    AgentRuntimeBackend, ConsoleCompletion, ConsoleExecutionBinding, ConsoleLedger,
    ConsoleOperationId, ConsoleOperationInput, ConsoleReplyStanding, ConsoleStanding,
    EngineClientEnvironment, LimitEnforcement, OciRuntimeBackend, RuntimeConfigurationBinding,
    RuntimeEnforcement, RuntimeLimits, RuntimeProviderStanding, RuntimeScope,
};
use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};
use mechanics::kinship::ProfileId;
use replica::body::{EncodingId, MutationModel, Schema, SchemaId, WorldId};
use runtime::exec::{
    AcceptRule, Access, Build, BuildId, Candidate, Effects, Handler, HandlerBinding,
    HandlerContext, Input, Limits as ExecLimits, Mode, Package, PayloadSpec, Resume, SchemaRef,
    Signature, Spec, Start, TerminalClass, TerminalRecord, TerminalSpec, Transcript,
    TranscriptBatch,
};
use runtime::plane::Activation;
use runtime::world::{
    AuthorityView, Context, Effect, Intent, LocalIdentity, Projection, Query, Rejection, World,
};

use super::agents::{AgentRegistry, AgentRuntimeMaterial};
use super::correspondence_host::CorrespondenceHost;

const WORLD: &str = "lait.agent.console";
const INTENT: &str = "lait.agent.console.execute";
const INPUT: &str = "lait.agent.console.input";
const OUTPUT: &str = "lait.agent.console.output";
const SPEC: &str = "lait.agent.console.command";
const ENCODING: &str = "lait.agent.console.intent.v1";
const SCHEMA_VERSION: u32 = 1;
const IMPLEMENTATION_VERSION: u32 = 1;
const MAX_COMMAND_BYTES: u32 = 64 * 1024;
const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
const PROVIDER_PROBE_INTERVAL_SECS: u64 = 30;
const MAX_CONCURRENT_CONSOLE_EXECUTIONS: usize = 4;
const MAX_CONCURRENT_EXECUTIONS_PER_AGENT: usize = 1;

fn world_id() -> WorldId {
    WorldId::parse(WORLD).expect("static Console World id")
}

fn implementation_id() -> [u8; 32] {
    *blake3::hash(b"lait.agent.console/world-implementation/1").as_bytes()
}

fn demand() -> Vec<u8> {
    AuthorizationDemand::require(
        PolicyCapability::new(WORLD, "execute"),
        Resource::root(WORLD),
    )
    .encode_canonical()
    .expect("static Console demand")
}

fn schema_ref(name: &str) -> SchemaRef {
    SchemaRef {
        name: SchemaId::parse(name).expect("static Console schema id"),
        version: SCHEMA_VERSION,
    }
}

fn exec_limits() -> ExecLimits {
    ExecLimits {
        attempts: 1,
        events: 16,
        checkpoints: 0,
        child_runs: 0,
        progress_bytes: MAX_OUTPUT_BYTES,
        checkpoint_bytes: 0,
        wall_millis: RuntimeLimits::default().wall_millis,
    }
}

fn console_spec() -> Spec {
    let required = demand();
    Spec {
        name: SchemaId::parse(SPEC).expect("static Console Spec"),
        version: SCHEMA_VERSION,
        access: Access {
            start: required.clone(),
            offer: required.clone(),
            control: required.clone(),
            accept: required.clone(),
            attach: None,
        },
        input: PayloadSpec {
            schema: schema_ref(INPUT),
            max_inline_bytes: MAX_COMMAND_BYTES,
            max_content_refs: 0,
            max_content_bytes: 0,
            read: required.clone(),
            max_additional_input_bytes: 0,
        },
        output: PayloadSpec {
            schema: schema_ref(OUTPUT),
            max_inline_bytes: 0,
            max_content_refs: 0,
            max_content_bytes: 0,
            read: required,
            max_additional_input_bytes: 0,
        },
        mode: Mode::Stream,
        terminal: TerminalSpec {
            transcript: Transcript::OnReturn,
            max_transcript_bytes: MAX_OUTPUT_BYTES,
            max_live_input_bytes: 0,
        },
        resume: Resume::Never,
        // A command may mutate the agent's persistent home. Returned is not
        // Accepted and a crash does not imply safe replay.
        effects: Effects::ExternalAtLeastOnce,
        accept: AcceptRule::Authorized,
        queries: Vec::new(),
        service: None,
        links: Vec::new(),
        limits: exec_limits(),
    }
}

#[derive(Clone)]
struct ConsoleWorld {
    schemas: Vec<Schema>,
    specs: Vec<Spec>,
}

impl ConsoleWorld {
    fn new() -> Self {
        Self {
            schemas: vec![Schema {
                id: SchemaId::parse(INTENT).expect("static Console intent"),
                version: SCHEMA_VERSION,
                encoding: EncodingId::parse(ENCODING).expect("static Console encoding"),
                mutation: MutationModel::Atomic,
                readable_predecessors: Vec::new(),
            }],
            specs: vec![console_spec()],
        }
    }
}

impl World for ConsoleWorld {
    fn descriptor(&self) -> runtime::world::Descriptor {
        runtime::world::Descriptor {
            id: self.id(),
            implementation_version: runtime::world::Version(IMPLEMENTATION_VERSION),
            schemas: self.schemas.clone(),
            limits: runtime::world::Limits {
                max_payload_bytes: MAX_COMMAND_BYTES,
            },
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: Vec::new(),
            find_extractors: Vec::new(),
            exec_specs: self.specs.clone(),
        }
    }

    fn id(&self) -> WorldId {
        world_id()
    }

    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    fn exec_specs(&self) -> &[Spec] {
        &self.specs
    }

    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        if intent.schema.as_str() != INTENT
            || intent.schema_version != SCHEMA_VERSION
            || intent.payload.is_empty()
            || intent.payload.len() > MAX_COMMAND_BYTES as usize
        {
            return Err(Rejection::InvalidRequest);
        }
        let build = console_build_id_for_payload_placeholder();
        Ok(Effect {
            content_refs: Vec::new(),
            exec: vec![runtime::exec::Cmd::Start(Start {
                spec: schema_ref(SPEC),
                build,
                input: Input {
                    inline: intent.payload,
                    content: Vec::new(),
                    content_bytes: 0,
                },
                parent: None,
                source: None,
                service: None,
                resources: Vec::new(),
                limits: exec_limits(),
                queries: Vec::new(),
                target: None,
            })],
            operations: Vec::new(),
            bodies: Vec::new(),
            effect: Vec::new(),
            declarations: Vec::new(),
            demand: demand(),
        })
    }

    fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        Err(Rejection::InvalidRequest)
    }
}

/// The World must emit the exact installed Build. It is fixed for this
/// implementation and agent Actor, so construction replaces this process-local
/// placeholder by giving the World the resolved id below.
fn console_build_id_for_payload_placeholder() -> BuildId {
    // Replaced by BoundConsoleWorld in production construction. Keeping this
    // impossible coordinate makes an accidentally unbound semantic package
    // fail selection instead of falling forward.
    BuildId::from_bytes([0; 32])
}

#[derive(Clone)]
struct BoundConsoleWorld {
    inner: ConsoleWorld,
    build: BuildId,
}

impl World for BoundConsoleWorld {
    fn descriptor(&self) -> runtime::world::Descriptor {
        self.inner.descriptor()
    }
    fn id(&self) -> WorldId {
        self.inner.id()
    }
    fn schemas(&self) -> &[Schema] {
        self.inner.schemas()
    }
    fn exec_specs(&self) -> &[Spec] {
        self.inner.exec_specs()
    }
    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        if intent.schema.as_str() != INTENT
            || intent.schema_version != SCHEMA_VERSION
            || intent.payload.is_empty()
            || intent.payload.len() > MAX_COMMAND_BYTES as usize
        {
            return Err(Rejection::InvalidRequest);
        }
        Ok(Effect {
            content_refs: Vec::new(),
            exec: vec![runtime::exec::Cmd::Start(Start {
                spec: schema_ref(SPEC),
                build: self.build,
                input: Input {
                    inline: intent.payload,
                    content: Vec::new(),
                    content_bytes: 0,
                },
                parent: None,
                source: None,
                service: None,
                resources: Vec::new(),
                limits: exec_limits(),
                queries: Vec::new(),
                target: None,
            })],
            operations: Vec::new(),
            bodies: Vec::new(),
            effect: Vec::new(),
            declarations: Vec::new(),
            demand: demand(),
        })
    }
    fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        Err(Rejection::InvalidRequest)
    }
}

struct ConsoleLifecycle;

impl world_sdk::WorldApplication for ConsoleLifecycle {
    fn founder_grants(&self) -> anyhow::Result<Vec<world_sdk::FounderGrant>> {
        Ok(vec![world_sdk::FounderGrant {
            capability: PolicyCapability::new(WORLD, "execute"),
            resource: Resource::root(WORLD),
            salt: *b"console-execute1",
        }])
    }
}

#[derive(Default)]
struct ConsoleExecutionGateState {
    total: usize,
    by_agent: BTreeMap<ProfileId, usize>,
}

/// Bounds aggregate host exposure independently of each container's own
/// limits. Acquisition is deliberately non-blocking: Runtime records a
/// durable failed Attempt instead of accumulating unbounded waiting workers.
struct ConsoleExecutionGate {
    total_limit: usize,
    per_agent_limit: usize,
    state: Mutex<ConsoleExecutionGateState>,
}

impl ConsoleExecutionGate {
    fn new(total_limit: usize, per_agent_limit: usize) -> Self {
        assert!(total_limit > 0);
        assert!(per_agent_limit > 0);
        Self {
            total_limit,
            per_agent_limit,
            state: Mutex::new(ConsoleExecutionGateState::default()),
        }
    }

    fn try_acquire(self: &Arc<Self>, agent: &ProfileId) -> Option<ConsoleExecutionPermit> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let agent_count = state.by_agent.get(agent).copied().unwrap_or(0);
        if state.total >= self.total_limit || agent_count >= self.per_agent_limit {
            return None;
        }
        state.total += 1;
        state.by_agent.insert(agent.clone(), agent_count + 1);
        Some(ConsoleExecutionPermit {
            gate: self.clone(),
            agent: agent.clone(),
        })
    }
}

struct ConsoleExecutionPermit {
    gate: Arc<ConsoleExecutionGate>,
    agent: ProfileId,
}

impl Drop for ConsoleExecutionPermit {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total = state.total.saturating_sub(1);
        match state.by_agent.get_mut(&self.agent) {
            Some(count) if *count > 1 => *count -= 1,
            Some(_) => {
                state.by_agent.remove(&self.agent);
            }
            None => {}
        }
    }
}

struct OciConsoleHandler {
    binding: HandlerBinding,
    build: Build,
    backend: Arc<dyn AgentRuntimeBackend>,
    agent: ProfileId,
    delegated_by: ProfileId,
    agent_home: PathBuf,
    image: String,
    configuration: RuntimeConfigurationBinding,
    execution_gate: Arc<ConsoleExecutionGate>,
}

impl Handler for OciConsoleHandler {
    fn binding(&self) -> &HandlerBinding {
        &self.binding
    }

    fn enforcement(&self) -> runtime::exec::Enforcement {
        runtime::exec::Enforcement::Container
    }

    fn handle(
        &self,
        context: &mut dyn HandlerContext,
    ) -> Result<Candidate, runtime::exec::Failure> {
        let _permit = self
            .execution_gate
            .try_acquire(&self.agent)
            .ok_or(runtime::exec::Failure::Os)?;
        let scope = RuntimeScope {
            agent: self.agent.clone(),
            delegated_by: self.delegated_by.clone(),
            agent_home: self.agent_home.clone(),
            run: data_encoding::HEXLOWER.encode(&context.run().as_bytes()),
            attempt: data_encoding::HEXLOWER.encode(&context.attempt().as_bytes()),
        };
        // This is the first provider mutation point and can only be reached
        // after Runtime committed Began and selected this exact Handler.
        let prepared = self
            .backend
            .prepare(&scope)
            .map_err(|_| runtime::exec::Failure::Os)?;
        if !prepared.verify_binding()
            || !prepared.clear_environment
            || prepared.agent != self.agent
            || prepared.delegated_by != self.delegated_by
            || !matches!(
                &prepared.provider,
                RuntimeProviderStanding::Ready { image, .. } if image == &self.image
            )
            || !enforcement_matches(&prepared.enforcement)
            || prepared.configuration != self.configuration
            || self.build.environment != self.configuration.as_bytes()
        {
            return Err(runtime::exec::Failure::InvalidContext);
        }
        let performer = runtime::exec::Subprocess::new(
            &console_spec(),
            &self.build,
            prepared.program.clone(),
            prepared.args.clone(),
            prepared.working_directory.clone(),
        )
        .with_environment(prepared.environment.clone());
        let outcome = performer.handle(context);
        let cleanup = self.backend.cleanup(&prepared);
        match (outcome, cleanup) {
            (result @ Err(_), _) => result,
            (Ok(_), Err(_)) => Err(runtime::exec::Failure::Os),
            (Ok(candidate), Ok(())) => Ok(candidate),
        }
    }
}

fn enforcement_matches(enforcement: &RuntimeEnforcement) -> bool {
    enforcement.read_only_root
        && enforcement.capabilities_dropped
        && enforcement.no_new_privileges
        && enforcement.network_none
        && enforcement.non_root_user == "podman-rootless-keep-id"
        && !enforcement.engine_socket_mounted
        && !enforcement.ambient_environment
        && !enforcement.ambient_working_directory
        && !enforcement.unrestricted_filesystem
        && !enforcement.secrets_mounted
        && !enforcement.externally_attested
        && enforcement.cpu == LimitEnforcement::OciEngine
        && enforcement.memory == LimitEnforcement::OciEngine
        && enforcement.wall == LimitEnforcement::OuterRuntime
        && enforcement.pids == LimitEnforcement::OciEngine
        && enforcement.open_files == LimitEnforcement::OciEngine
        && enforcement.single_file_size == LimitEnforcement::OciEngine
        && enforcement.output == LimitEnforcement::OuterRuntime
        && enforcement.limits == RuntimeLimits::default()
}

fn build_for(
    actor: mechanics::ids::ActorId,
    seed: &[u8; 32],
    configuration: RuntimeConfigurationBinding,
) -> Result<Build, String> {
    Build {
        id: BuildId::from_bytes([0; 32]),
        world: world_id(),
        world_build: implementation_id(),
        spec: schema_ref(SPEC),
        handler: replica::content::ContentRef {
            content_id: *blake3::hash(b"lait.agent.console/oci-handler/1").as_bytes(),
        },
        dependencies: None,
        environment: configuration.as_bytes(),
        config: Vec::new(),
        checkpoint: None,
        replay_commands: None,
        compatible_from: Vec::new(),
        publisher: actor,
        signature: Signature {
            signer: mechanics::actor::device_from_seed(seed),
            algorithm: 1,
            bytes: [0; 64],
        },
    }
    .sign(seed)
    .map_err(|error| format!("sign Console Build: {error}"))
}

struct ConsoleNode {
    material: AgentRuntimeMaterial,
    service: Arc<super::correspondence::CorrespondenceService>,
    station: runtime::Station,
    identity: LocalIdentity,
    package: Package,
    build: Build,
    image: String,
    backend: Arc<dyn AgentRuntimeBackend>,
    configuration: RuntimeConfigurationBinding,
}

impl ConsoleNode {
    fn form(
        material: AgentRuntimeMaterial,
        service: Arc<super::correspondence::CorrespondenceService>,
        backend: Arc<dyn AgentRuntimeBackend>,
        image: String,
        execution_gate: Arc<ConsoleExecutionGate>,
    ) -> Result<Self, String> {
        let provider = backend.probe();
        let configuration = backend
            .configuration_binding(&provider)
            .map_err(|error| format!("bind Console provider configuration: {error}"))?;
        let private_home = material.home.join("agent").join("console-space");
        let semantic = Arc::new(ConsoleWorld::new());
        let lifecycle = Arc::new(ConsoleLifecycle);
        let semantic_packages = crate::orbital::WorldPackages::new().with_package(
            crate::orbital::WorldPackage::new(semantic, implementation_id())
                .with_exec(Package::new().with_spec(console_spec()))
                .with_lifecycle(lifecycle.clone()),
        );
        let (authority, coordinates) = crate::orbital::form_space(
            &semantic_packages,
            &private_home,
            &material.seed,
            "Agent Console",
        )
        .map_err(|error| format!("form private Console Space: {error:#}"))?;
        let device = mechanics::actor::device_from_seed(&material.seed);
        let actor = authority
            .resolve(&device)
            .ok_or_else(|| "agent device has no Actor in its private Console Space".to_string())?
            .actor;
        let build = build_for(actor, &material.seed, configuration)?;
        let handler = Arc::new(OciConsoleHandler {
            binding: HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: build.handler.clone(),
                role: None,
                links: Vec::new(),
            },
            build: build.clone(),
            backend: backend.clone(),
            agent: material.agent.clone(),
            delegated_by: material.owner.clone(),
            agent_home: material.home.clone(),
            image: image.clone(),
            configuration,
            execution_gate,
        });
        let exec = Package::new()
            .with_spec(console_spec())
            .with_build(build.clone())
            .with_handler(handler);
        let bound = Arc::new(BoundConsoleWorld {
            inner: ConsoleWorld::new(),
            build: build.id,
        });
        let packages = crate::orbital::WorldPackages::new().with_package(
            crate::orbital::WorldPackage::new(bound, implementation_id())
                .with_exec(exec.clone())
                .with_lifecycle(lifecycle),
        );
        let (catalog, _) = packages
            .build()
            .map_err(|error| format!("compose Console World: {error:?}"))?;
        let runtime = runtime::Runtime::open(
            crate::orbital::orbital_store_root(&private_home),
            catalog,
            Arc::new(authority.clone()),
            Arc::new(authority),
        );
        let station = runtime
            .materialize(&coordinates)
            .map_err(|error| format!("materialize private Console Space: {error:?}"))?
            .open(Activation::offline())
            .map_err(|error| format!("activate private Console Space: {error:?}"))?;
        // Sending is an external at-least-once effect. A claimed send cannot
        // be replayed after restart. Runtime dispatches are deliberately not
        // changed here; reconcile them against the durable Run DAG below.
        ConsoleLedger::at(&material.home)
            .recover_reply_sends(mechanics::wallclock::now_secs())
            .map_err(|error| format!("recover Console reply outbox: {error}"))?;
        Ok(Self {
            identity: runtime::Runtime::identity_from_seed(&material.seed),
            material,
            service,
            station,
            package: exec,
            build,
            image,
            backend,
            configuration,
        })
    }

    fn provider_matches(&self, standing: &RuntimeProviderStanding) -> bool {
        self.backend
            .configuration_binding(standing)
            .is_ok_and(|binding| binding == self.configuration)
    }

    fn shutdown(self) {
        let _ = self.station.vacate();
    }

    fn tick(&self, now: u64) -> Result<(), String> {
        // Inbox admission and durable Run reconciliation are independent.
        // Reaching a ledger bound (or one malformed new message) must never
        // prevent already-dispatched work from being observed and replied to.
        let admission = self.accept_messages();
        let session = self
            .station
            .dock(&world_id(), &self.identity)
            .map_err(|error| format!("dock Console World: {error:?}"))?;
        // Admission and dispatch are separate durable steps. In particular,
        // an Accepted operation survives a daemon crash between filing the
        // inbox item and claiming its effect, and is driven from the ledger on
        // the next tick rather than being hidden by inbox de-duplication.
        self.dispatch_accepted(&session, now)?;
        session
            .perform(&self.package, |bytes| {
                let mut reader = std::io::Cursor::new(bytes);
                self.station
                    .content_write(
                        &self.identity,
                        runtime::world::RequestId::mint().as_bytes(),
                        &mut reader,
                    )
                    .map_err(|error| runtime::world::Failure::PersistenceCause {
                        operation: "agent.console.transcript".into(),
                        reason: error.to_string(),
                    })
            })
            .map_err(|error| format!("perform Console Run: {error}"))?;
        self.reconcile(&session, now)?;
        admission
    }

    fn accept_messages(&self) -> Result<(), String> {
        let ledger = ConsoleLedger::at(&self.material.home);
        ledger
            .compact_finalized()
            .map_err(|error| error.to_string())?;
        let mut known = ledger
            .known_operation_ids()
            .map_err(|error| error.to_string())?;
        let mut messages = self.service.opened_messages()?;
        messages.sort_by(|left, right| left.deposit_id.cmp(&right.deposit_id));
        for message in messages {
            if !message.provenance_agrees
                || message.sender.as_ref() != Some(&self.material.owner)
                || message.body.is_empty()
                || message.body.len() > agent::MAX_CONSOLE_INPUT_BYTES
            {
                continue;
            }
            let operation = operation_id(&self.material.agent, &message.deposit_id, &message.body);
            if known.contains(&operation) {
                continue;
            }
            let request = runtime::world::RequestId::from_bytes(operation.0);
            let run = runtime::exec::derive_run_id(
                self.station.space_id(),
                &world_id(),
                self.identity.device(),
                request.as_bytes(),
                0,
            );
            let digest = blake3::hash(message.deposit_id.as_bytes());
            let mut sequence = [0u8; 8];
            sequence.copy_from_slice(&digest.as_bytes()[..8]);
            let input = ConsoleOperationInput {
                id: operation,
                sender: self.material.owner.clone(),
                agent: self.material.agent.clone(),
                generation: 1,
                sequence: u64::from_be_bytes(sequence),
                payload: message.body.as_bytes().to_vec(),
                accepted_at: message.sent_at,
                execution: ConsoleExecutionBinding {
                    space: self.station.space_id().as_str().to_owned(),
                    world: WORLD.into(),
                    world_implementation: implementation_id(),
                    spec: SPEC.into(),
                    spec_version: SCHEMA_VERSION,
                    build: self.build.id.as_bytes(),
                    image: self.image.clone(),
                    enforcement: self.build.environment,
                    run: run.as_bytes(),
                },
            };
            match ledger.accept(&self.material.ownership, input) {
                Ok(_) => {}
                Err(agent::Error::Bound("console operations"))
                | Err(agent::Error::Bound("consumed console operations")) => {
                    tracing::warn!(
                        agent = %self.material.agent,
                        "Console inbox capacity reached; existing Runs will still reconcile"
                    );
                    break;
                }
                Err(error) => return Err(error.to_string()),
            }
            known.insert(operation);
        }
        Ok(())
    }

    fn dispatch_accepted(&self, session: &runtime::Session, now: u64) -> Result<(), String> {
        let ledger = ConsoleLedger::at(&self.material.home);
        for operation in ledger.list().map_err(|error| error.to_string())? {
            if !matches!(operation.standing, ConsoleStanding::Accepted) {
                continue;
            }
            let request = runtime::world::RequestId::from_bytes(operation.input.id.0);
            let expected_run = runtime::exec::derive_run_id(
                self.station.space_id(),
                &world_id(),
                self.identity.device(),
                request.as_bytes(),
                0,
            );
            let binding = &operation.input.execution;
            if binding.space != self.station.space_id().as_str()
                || binding.world != WORLD
                || binding.world_implementation != implementation_id()
                || binding.spec != SPEC
                || binding.spec_version != SCHEMA_VERSION
                || binding.build != self.build.id.as_bytes()
                || binding.image != self.image
                || binding.enforcement != self.build.environment
                || binding.run != expected_run.as_bytes()
            {
                return Err(
                    "Accepted Console operation no longer matches its execution binding".into(),
                );
            }
            // Claim before the signed Start can become durable. A crash after
            // this claim is OutcomeUnknown, never authority to guess and run
            // the external effect again.
            ledger
                .claim_dispatch(operation.input.id, now)
                .map_err(|error| error.to_string())?;
            self.submit_operation(session, &operation)?;
        }
        Ok(())
    }

    fn submit_operation(
        &self,
        session: &runtime::Session,
        operation: &agent::ConsoleOperation,
    ) -> Result<(), String> {
        let request = runtime::world::RequestId::from_bytes(operation.input.id.0);
        let intent = Intent {
            schema: SchemaId::parse(INTENT).expect("static Console intent"),
            schema_version: SCHEMA_VERSION,
            payload: operation.input.payload.clone(),
        };
        let action = self
            .identity
            .sign_action(session, request, intent)
            .map_err(|error| format!("sign Console action: {error}"))?;
        session
            .submit(action)
            .map_err(|error| format!("commit Console Start: {error}"))?;
        Ok(())
    }

    fn reconcile(&self, session: &runtime::Session, now: u64) -> Result<(), String> {
        let ledger = ConsoleLedger::at(&self.material.home);
        for operation in ledger.list().map_err(|error| error.to_string())? {
            let ConsoleStanding::Dispatched { attempt, .. } = operation.standing else {
                self.deliver_reply(&ledger, &operation, now)?;
                continue;
            };
            let run = runtime::exec::RunId::from_bytes(operation.input.execution.run);
            let state = match session.work(
                runtime::exec::WorkRequest::Inspect {
                    world: world_id(),
                    run,
                },
                operation.input.id.0,
            ) {
                Ok(runtime::exec::WorkReply::State(state)) => state,
                Ok(_) => continue,
                Err(runtime::exec::WorkRefusal::NotFound(_)) if attempt.is_none() => {
                    let request = runtime::world::RequestId::from_bytes(operation.input.id.0);
                    let intent = Intent {
                        schema: SchemaId::parse(INTENT).expect("static Console intent"),
                        schema_version: SCHEMA_VERSION,
                        payload: operation.input.payload.clone(),
                    };
                    match session
                        .operation_status_for(request, &intent)
                        .map_err(|error| format!("reconcile Console Start receipt: {error}"))?
                    {
                        runtime::world::OperationStatus::Absent => {
                            self.submit_operation(session, &operation)?;
                        }
                        runtime::world::OperationStatus::Found { .. } => {}
                    }
                    continue;
                }
                Err(runtime::exec::WorkRefusal::NotFound(_)) => {
                    ledger
                        .mark_outcome_unknown(operation.input.id, 0, now)
                        .map_err(|error| error.to_string())?;
                    continue;
                }
                Err(error) => return Err(format!("inspect Console Run: {error}")),
            };
            if state.world != world_id()
                || state.run.as_bytes() != operation.input.execution.run
                || state.build.as_bytes() != operation.input.execution.build
                || state.spec != schema_ref(SPEC)
                || state.device != *self.identity.device()
                || state.invoker != self.build.publisher
            {
                return Err("Console Run coordinates changed after acceptance".into());
            }
            let Some(runtime_attempt) = state.attempts.first() else {
                continue;
            };
            if attempt.is_none() {
                ledger
                    .bind_attempt(operation.input.id, runtime_attempt.attempt.as_bytes())
                    .map_err(|error| error.to_string())?;
            }
            if let Some(returned) = runtime_attempt.returned.first() {
                let (body, cursor, exit_code) = match &returned.transcript {
                    Some(reference) => {
                        self.read_transcript(reference, run, runtime_attempt.attempt)?
                    }
                    None => {
                        ledger
                            .mark_outcome_unknown(operation.input.id, 0, now)
                            .map_err(|error| error.to_string())?;
                        continue;
                    }
                };
                let reply = render_return(operation.input.id, body, returned.terminal, exit_code);
                let completed = ledger
                    .complete_with_reply(
                        operation.input.id,
                        ConsoleCompletion {
                            attempt: runtime_attempt.attempt.as_bytes(),
                            transcript_cursor: cursor,
                            exit_code,
                            completed_at: now,
                        },
                        reply.into_bytes(),
                        now,
                    )
                    .map_err(|error| error.to_string())?;
                self.deliver_reply(&ledger, &completed, now)?;
            } else if let Some(failed) = runtime_attempt.failed.first() {
                let class = format!("{:?}", failed.class);
                let reply = render_reply(
                    operation.input.id,
                    "failed",
                    format!("Command failed before returning output ({class}).").as_bytes(),
                );
                let failed = ledger
                    .fail_with_reply(
                        operation.input.id,
                        runtime_attempt.attempt.as_bytes(),
                        class.clone(),
                        now,
                        reply.into_bytes(),
                        now,
                    )
                    .map_err(|error| error.to_string())?;
                self.deliver_reply(&ledger, &failed, now)?;
            }
        }
        for operation in ledger.list().map_err(|error| error.to_string())? {
            self.deliver_reply(&ledger, &operation, now)?;
        }
        Ok(())
    }

    fn read_transcript(
        &self,
        reference: &replica::content::ContentRef,
        run: runtime::exec::RunId,
        attempt: runtime::exec::AttemptId,
    ) -> Result<(Vec<u8>, u64, Option<i32>), String> {
        let status = self
            .station
            .content_stat(&self.identity, reference)
            .map_err(|error| format!("stat Console transcript: {error}"))?;
        let len = usize::try_from(status.plaintext_len)
            .map_err(|_| "Console transcript is too large for this host".to_string())?;
        if len > agent::MAX_CONSOLE_REPLY_BYTES {
            return Err("Console transcript exceeds the reply bound".into());
        }
        let bytes = self
            .station
            .content_read(&self.identity, reference, 0, len)
            .map_err(|error| format!("read Console transcript: {error}"))?;
        let batch = TranscriptBatch::decode_canonical(&bytes)
            .map_err(|error| format!("decode Console transcript: {error:?}"))?;
        if batch.run != run || batch.attempt != attempt {
            return Err("Console transcript belongs to another Run or Attempt".into());
        }
        let mut output = Vec::new();
        let mut cursor = 0;
        let mut exit_code = None;
        for record in batch.records {
            match record {
                TerminalRecord::Output { end, bytes, .. } => {
                    output.extend_from_slice(&bytes);
                    cursor = cursor.max(end);
                }
                TerminalRecord::ProcessExited { at, code } => {
                    cursor = cursor.max(at);
                    exit_code = code;
                }
                TerminalRecord::Suppressed { dropped_bytes, .. }
                | TerminalRecord::Gap { dropped_bytes, .. } => output.extend_from_slice(
                    format!("\n[output truncated: {dropped_bytes} bytes]\n").as_bytes(),
                ),
                TerminalRecord::Resized { at, .. } | TerminalRecord::AttemptEnded { at, .. } => {
                    cursor = cursor.max(at);
                }
            }
        }
        output.truncate(agent::MAX_CONSOLE_REPLY_BYTES);
        Ok((output, cursor, exit_code))
    }

    fn deliver_reply(
        &self,
        ledger: &ConsoleLedger,
        operation: &agent::ConsoleOperation,
        now: u64,
    ) -> Result<(), String> {
        let body = match &operation.reply {
            ConsoleReplyStanding::Prepared { body, .. } => body.clone(),
            ConsoleReplyStanding::None
                if matches!(operation.standing, ConsoleStanding::OutcomeUnknown { .. }) =>
            {
                let body = render_reply(
                    operation.input.id,
                    "outcome unknown",
                    b"The command outcome is unknown after recovery; it was not run again.",
                )
                .into_bytes();
                ledger
                    .prepare_reply(operation.input.id, body.clone(), now)
                    .map_err(|error| error.to_string())?;
                body
            }
            _ => return Ok(()),
        };
        ledger
            .claim_reply_send(operation.input.id, now)
            .map_err(|error| error.to_string())?;
        let body = String::from_utf8_lossy(&body).into_owned();
        let deposit = match self.service.send_message(&self.material.owner, body, now) {
            Ok(deposit) => deposit,
            Err(error) => {
                ledger
                    .mark_reply_outcome_unknown(operation.input.id, now)
                    .map_err(|ledger_error| {
                        format!(
                            "{error}; additionally failed to seal ambiguous Console reply: {ledger_error}"
                        )
                    })?;
                return Err(error);
            }
        };
        ledger
            .mark_reply_sent(operation.input.id, deposit, now)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn operation_id(agent: &ProfileId, deposit: &str, body: &str) -> ConsoleOperationId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait.agent.console/operation/1");
    hasher.update(agent.as_str().as_bytes());
    hasher.update(deposit.as_bytes());
    hasher.update(blake3::hash(body.as_bytes()).as_bytes());
    let digest = hasher.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    ConsoleOperationId(id)
}

fn render_return(
    operation: ConsoleOperationId,
    bytes: Vec<u8>,
    terminal: TerminalClass,
    code: Option<i32>,
) -> String {
    let status = match (terminal, code) {
        (TerminalClass::Succeeded, Some(0) | None) => "completed",
        _ => "failed",
    };
    if bytes.is_empty() {
        match (terminal, code) {
            (TerminalClass::Succeeded, Some(0) | None) => {
                render_reply(operation, status, b"Command completed with no output.")
            }
            (_, Some(code)) => render_reply(
                operation,
                status,
                format!("Command exited with status {code} and no output.").as_bytes(),
            ),
            _ => render_reply(
                operation,
                status,
                b"Command completed without output and without an exit status.",
            ),
        }
    } else {
        render_reply(operation, status, &bytes)
    }
}

fn render_reply(operation: ConsoleOperationId, status: &str, body: &[u8]) -> String {
    let coordinate = data_encoding::HEXLOWER.encode(&operation.0[..4]);
    let header = format!("Work {coordinate} · {status}\n\n");
    let budget = agent::MAX_CONSOLE_REPLY_BYTES.saturating_sub(header.len());
    let mut rendered = String::from_utf8_lossy(body).into_owned();
    if rendered.len() > budget {
        let marker = "\n[reply truncated]";
        let keep = budget.saturating_sub(marker.len());
        let mut boundary = keep.min(rendered.len());
        while boundary > 0 && !rendered.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        rendered.truncate(boundary);
        if marker.len() <= budget.saturating_sub(rendered.len()) {
            rendered.push_str(marker);
        }
    }
    format!("{header}{rendered}")
}

fn configured_backend(
    agents_root: &Path,
) -> Result<(Arc<dyn AgentRuntimeBackend>, String), String> {
    let engine = std::env::var_os("LAIT_AGENT_OCI_ENGINE")
        .map(PathBuf::from)
        .ok_or_else(|| "LAIT_AGENT_OCI_ENGINE is not configured".to_string())?;
    let image = std::env::var("LAIT_AGENT_OCI_IMAGE")
        .map_err(|_| "LAIT_AGENT_OCI_IMAGE is not configured".to_string())?;
    let home = std::env::var_os("LAIT_AGENT_OCI_CLIENT_HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "LAIT_AGENT_OCI_CLIENT_HOME is not configured".to_string())?;
    let xdg_runtime_dir = std::env::var_os("LAIT_AGENT_OCI_XDG_RUNTIME_DIR").map(PathBuf::from);
    let backend = OciRuntimeBackend::new(
        engine,
        image.clone(),
        agents_root.to_path_buf(),
        RuntimeLimits::default(),
        EngineClientEnvironment {
            home,
            xdg_runtime_dir,
        },
    )
    .map_err(|error| error.to_string())?;
    backend
        .scavenge_stale_attempts()
        .map_err(|error| format!("scavenge stale agent attempts: {error}"))?;
    Ok((Arc::new(backend), image))
}

/// Long-lived owner-local supervisor. Nodes retain their Station activation so
/// activation-owned Attempt workers and transcript handoff cannot be orphaned
/// between collection passes.
pub(crate) struct AgentConsoleSupervisor {
    registry: AgentRegistry,
    correspondence: Arc<CorrespondenceHost>,
    configured: Result<(Arc<dyn AgentRuntimeBackend>, String), String>,
    nodes: BTreeMap<String, ConsoleNode>,
    ready: BTreeMap<String, bool>,
    next_probe: BTreeMap<String, u64>,
    execution_gate: Arc<ConsoleExecutionGate>,
}

impl AgentConsoleSupervisor {
    pub(crate) fn new(
        registry: AgentRegistry,
        correspondence: Arc<CorrespondenceHost>,
        agents_root: PathBuf,
    ) -> Self {
        let configured = configured_backend(&agents_root);
        Self {
            registry,
            correspondence,
            configured,
            nodes: BTreeMap::new(),
            ready: BTreeMap::new(),
            next_probe: BTreeMap::new(),
            execution_gate: Arc::new(ConsoleExecutionGate::new(
                MAX_CONCURRENT_CONSOLE_EXECUTIONS,
                MAX_CONCURRENT_EXECUTIONS_PER_AGENT,
            )),
        }
    }

    pub(crate) fn tick(&mut self, now: u64) {
        for loaded in self.correspondence.loaded_agents() {
            if !matches!(
                self.registry.console_runtime_enabled(&loaded.name),
                Ok(true)
            ) {
                if let Some(node) = self.nodes.remove(&loaded.name) {
                    node.shutdown();
                }
                self.ready.remove(&loaded.name);
                self.next_probe.remove(&loaded.name);
                super::agents::set_live_runtime_standing(
                    loaded.profile.clone(),
                    agent::PrimitiveStanding::Unavailable,
                );
                continue;
            }
            if !self.nodes.contains_key(&loaded.name) {
                let node = (|| -> Result<ConsoleNode, String> {
                    let (backend, image) =
                        self.configured.as_ref().map_err(|error| (*error).clone())?;
                    let material = self
                        .registry
                        .runtime_material(&loaded.name)
                        .map_err(|error| error.to_string())?;
                    let service = self.correspondence.agent(&loaded.name, now)?;
                    ConsoleNode::form(
                        material,
                        service,
                        backend.clone(),
                        image.clone(),
                        self.execution_gate.clone(),
                    )
                })();
                match node {
                    Ok(node) => {
                        self.nodes.insert(loaded.name.clone(), node);
                        self.ready.insert(loaded.name.clone(), true);
                        self.next_probe.insert(
                            loaded.name.clone(),
                            now.saturating_add(PROVIDER_PROBE_INTERVAL_SECS),
                        );
                        super::agents::set_live_runtime_standing(
                            loaded.profile.clone(),
                            agent::PrimitiveStanding::Ready,
                        );
                    }
                    Err(error) => {
                        super::agents::set_live_runtime_standing(
                            loaded.profile.clone(),
                            agent::PrimitiveStanding::Unavailable,
                        );
                        tracing::debug!(agent = %loaded.name, %error, "agent Console remains unavailable");
                        continue;
                    }
                }
            }
            if now >= self.next_probe.get(&loaded.name).copied().unwrap_or(0) {
                if let Some(node) = self.nodes.get(&loaded.name) {
                    let standing = node.backend.probe();
                    let ready = standing.is_ready() && node.provider_matches(&standing);
                    self.ready.insert(loaded.name.clone(), ready);
                    self.next_probe.insert(
                        loaded.name.clone(),
                        now.saturating_add(PROVIDER_PROBE_INTERVAL_SECS),
                    );
                    super::agents::set_live_runtime_standing(
                        loaded.profile.clone(),
                        if ready {
                            agent::PrimitiveStanding::Ready
                        } else {
                            agent::PrimitiveStanding::Unavailable
                        },
                    );
                }
            }
            if !self.ready.get(&loaded.name).copied().unwrap_or(false) {
                continue;
            }
            if let Some(node) = self.nodes.get(&loaded.name) {
                if let Err(error) = node.tick(now) {
                    self.next_probe.insert(loaded.name.clone(), now);
                    tracing::warn!(agent = %loaded.name, %error, "agent Console tick failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_podman(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let engine = root.join("fake-podman");
        std::fs::write(
            &engine,
            br#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    info)
      printf '%s\n' '{"host":{"security":{"rootless":true,"seccompEnabled":true},"serviceIsRemote":false}}'
      exit 0
      ;;
    image|rm)
      exit 0
      ;;
    run)
      exec /bin/sh -s
      ;;
  esac
done
exit 125
"#,
        )
        .expect("write fake Podman");
        let mut permissions = std::fs::metadata(&engine)
            .expect("fake Podman metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&engine, permissions).expect("make fake Podman executable");
        engine
    }

    #[test]
    fn reply_rendering_is_utf8_safe_and_never_exceeds_the_transport_bound() {
        let operation = ConsoleOperationId([0xab; 16]);
        let binary = vec![0xff; agent::MAX_CONSOLE_REPLY_BYTES];
        let reply = render_reply(operation, "completed", &binary);
        assert!(reply.len() <= agent::MAX_CONSOLE_REPLY_BYTES);
        assert!(reply.starts_with("Work abababab · completed\n\n"));
        assert!(reply.ends_with("[reply truncated]"));
    }

    #[test]
    fn ordinary_replies_carry_a_stable_operation_coordinate() {
        let first = render_reply(
            ConsoleOperationId([1; 16]),
            "completed",
            b"second to finish",
        );
        let second = render_reply(ConsoleOperationId([2; 16]), "completed", b"first to finish");
        assert!(first.starts_with("Work 01010101 · completed"));
        assert!(second.starts_with("Work 02020202 · completed"));
        assert_ne!(first.lines().next(), second.lines().next());
    }

    #[test]
    fn execution_gate_bounds_total_and_per_agent_work() {
        let gate = Arc::new(ConsoleExecutionGate::new(2, 1));
        let adam = ProfileId::from_genesis(b"adam");
        let eve = ProfileId::from_genesis(b"eve");
        let third = ProfileId::from_genesis(b"third");

        let adam_permit = gate.try_acquire(&adam).expect("Adam permit");
        assert!(gate.try_acquire(&adam).is_none(), "one Attempt per agent");
        let eve_permit = gate.try_acquire(&eve).expect("Eve permit");
        assert!(gate.try_acquire(&third).is_none(), "aggregate cap");
        drop(adam_permit);
        let third_permit = gate.try_acquire(&third).expect("released permit");
        drop((eve_permit, third_permit));
        assert!(gate.try_acquire(&adam).is_some(), "all permits returned");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owner_correspondence_runs_through_exec_and_returns_to_the_same_conversation() {
        let temporary = tempfile::tempdir().expect("temporary identity root");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical test root");
        let owner_home = root.join("owner");
        mechanics::secretfs::create_private_dir(&owner_home).expect("owner home");
        let owner = crate::config::identity_profile(&owner_home).expect("owner profile");
        let registry = AgentRegistry::new(root.clone(), owner_home.clone());
        let created = registry
            .create(
                super::super::agents::ManagementRequest {
                    requester: &owner,
                    act_as: None,
                },
                "adam",
                "Adam is a virtual assistant.",
            )
            .expect("create Adam");

        let agents_root = crate::registry::agents_base(&root);
        let host = CorrespondenceHost::open(&owner_home, &agents_root, true);
        let primary = host.primary();
        primary.restore(1).expect("restore owner correspondence");
        let adam = host.agent("adam", 1).expect("restore Adam correspondence");
        host.introduce_agent("adam", 1)
            .expect("introduce owner and Adam");
        let carrier = correspondence::SharedMem::new();
        primary
            .carry_over_with(Box::new(carrier.clone()), None, 1)
            .expect("carry owner correspondence");
        adam.carry_over_with(Box::new(carrier), None, 1)
            .expect("carry Adam correspondence");

        let sent = primary
            .handle(crate::control::Request::CorrespondSend {
                to: created.state.record.ownership.agent().as_str().to_owned(),
                body: "printf 'fixture-e2e\\n'".into(),
            })
            .await;
        assert!(matches!(sent, crate::control::Response::Reach(_)));
        host.collect_loaded(2).expect("collect command for Adam");

        let client_home = root.join("podman-client");
        mechanics::secretfs::create_private_dir(&client_home).expect("Podman client home");
        let image = format!("example.invalid/lait-agent@sha256:{}", "a".repeat(64));
        let backend: Arc<dyn AgentRuntimeBackend> = Arc::new(
            OciRuntimeBackend::new(
                fake_podman(&root),
                image.clone(),
                agents_root,
                RuntimeLimits::default(),
                EngineClientEnvironment {
                    home: client_home,
                    xdg_runtime_dir: None,
                },
            )
            .expect("test OCI backend"),
        );
        let execution_gate = Arc::new(ConsoleExecutionGate::new(
            MAX_CONCURRENT_CONSOLE_EXECUTIONS,
            MAX_CONCURRENT_EXECUTIONS_PER_AGENT,
        ));
        let node = ConsoleNode::form(
            registry.runtime_material("adam").expect("Adam runtime"),
            adam.clone(),
            backend.clone(),
            image.clone(),
            execution_gate.clone(),
        )
        .expect("form Adam Console");

        // Model the durable seam immediately after inbox admission. A crash at
        // this point used to strand the command because the next inbox scan
        // recognized its id but nothing drove the persisted Accepted record.
        node.accept_messages().expect("admit Adam command");
        assert!(matches!(
            ConsoleLedger::at(&node.material.home)
                .list()
                .expect("read admitted command")[0]
                .standing,
            ConsoleStanding::Accepted
        ));
        // Model the narrower crash after the effect claim but before Runtime's
        // Start commit. Recovery may submit only after the durable receipt
        // index proves this exact request and payload absent.
        let admitted_id = ConsoleLedger::at(&node.material.home)
            .list()
            .expect("read accepted command")[0]
            .input
            .id;
        let claimed = ConsoleLedger::at(&node.material.home)
            .claim_dispatch(admitted_id, 3)
            .expect("claim command without submitting it");
        assert!(matches!(
            claimed.standing,
            ConsoleStanding::Dispatched { attempt: None, .. }
        ));

        let mut replied = false;
        for tick in 4..80 {
            node.tick(tick).expect("advance Console");
            host.collect_loaded(tick).expect("collect Console reply");
            if primary
                .opened_messages()
                .expect("owner inbox")
                .iter()
                .any(|message| message.body.contains("fixture-e2e"))
            {
                replied = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            replied,
            "the Exec result never returned through correspondence"
        );
        let reply = primary
            .opened_messages()
            .expect("owner inbox")
            .into_iter()
            .find(|message| message.body.contains("fixture-e2e"))
            .expect("Console reply");
        assert!(reply.provenance_agrees);
        assert_eq!(
            reply.sender.as_ref(),
            Some(created.state.record.ownership.agent()),
            "the agent, not its owner, must author the reply"
        );

        let operations = ConsoleLedger::at(&node.material.home)
            .list()
            .expect("Console ledger");
        let [operation] = operations.as_slice() else {
            panic!("exactly one Console operation must be durable");
        };
        assert_eq!(
            operation.input.execution.space,
            node.station.space_id().as_str()
        );
        assert_eq!(operation.input.execution.world, WORLD);
        assert_eq!(operation.input.execution.build, node.build.id.as_bytes());
        assert_eq!(
            operation.input.execution.enforcement,
            node.build.environment
        );
        let session = node
            .station
            .dock(&world_id(), &node.identity)
            .expect("dock Console World");
        let state = match session
            .work(
                runtime::exec::WorkRequest::Inspect {
                    world: world_id(),
                    run: runtime::exec::RunId::from_bytes(operation.input.execution.run),
                },
                operation.input.id.0,
            )
            .expect("inspect Console Run")
        {
            runtime::exec::WorkReply::State(state) => state,
            other => panic!("unexpected Console work reply: {other:?}"),
        };
        assert_eq!(state.device, *node.identity.device());
        assert_eq!(state.invoker, node.build.publisher);
        assert_eq!(state.build, node.build.id);
        assert!(state
            .attempts
            .iter()
            .any(|attempt| !attempt.returned.is_empty()));
        drop(session);
        node.shutdown();
        let reopened = ConsoleNode::form(
            registry.runtime_material("adam").expect("Adam runtime"),
            adam,
            backend,
            image,
            execution_gate,
        )
        .expect("reopen Adam Console Space");
        reopened.tick(80).expect("advance reopened Console");
        host.collect_loaded(80)
            .expect("collect after Console reopen");
        assert_eq!(
            primary
                .opened_messages()
                .expect("owner inbox after reopen")
                .iter()
                .filter(|message| message.body.contains("fixture-e2e"))
                .count(),
            1,
            "reopening the private Console Space never replays work or its reply"
        );
        reopened.shutdown();
    }
}
