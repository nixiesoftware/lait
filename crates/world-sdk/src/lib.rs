#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::todo,
        clippy::panic
    )
)]

//! The typed seam between Runtime and an independently shipped World process.
//!
//! Runtime remains the authority: it owns the principal, immutable snapshots,
//! Find capabilities, signing, admission, and durable commit. The child gets
//! only the same bounded [`runtime::world::Context`] facade the semantic World
//! contract defines. Calls back through that facade are correlated to the invocation;
//! a deliberately detached Find lease may outlive it and pins both its exact
//! publication and the runner generation until the product releases it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use replica::body::{BodyKey, SchemaId, WorldId};
use runtime::world::call::{
    Access as CallAccess, Call as ApplicationCall, Context as ApplicationContext,
    Failure as CallFailure, Handler, IdentityAccess, Nudge, Reply as ApplicationReply,
    SessionAccess,
};
use runtime::world::{
    AnalyticalMemoryLease, AnalyticalMemoryReservation, BodyBytes, BodyReadFailure, BodyReader,
    CollaborativeBody, ContentStatus, Context, Descriptor, Effect, ExtractionContext, FindHandle,
    FindLease, FindReader, HostedAnalyticalMemoryLease, HostedAnalyticalMemoryReservation, Intent,
    LifecycleSourceCoordinate, OutcomeFacts, PrincipalFacts, Projection, Query, Rejection, World,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use world_runner::{
    CallbackHandler, Host, Instance, Operation, Reply, RequestClient, Service, ServiceDescriptor,
};

/// Protocol generation of the typed World service operations in this crate.
pub const ABI_VERSION: u32 = 3;

const DESCRIBE: &str = "semantic.describe";
const SUBMIT: &str = "semantic.submit";
const QUERY: &str = "semantic.query";
const EXTRACT: &str = "semantic.extract";
const APPLICATION_ACCESS: &str = "application.access";
const APPLICATION_CALL: &str = "application.call";
const APPLICATION_NUDGES: &str = "application.nudges";
const APPLICATION_FOUNDER_GRANTS: &str = "application.founder_grants";
const APPLICATION_ADMISSION_EVIDENCE: &str = "application.admission_evidence";
const APPLICATION_INITIAL_SCOPE: &str = "application.initial_scope";
const APPLICATION_BOOTSTRAP: &str = "application.bootstrap";
const APPLICATION_ASSESS_UPGRADE: &str = "application.assess_upgrade";
const APPLICATION_VERIFICATION_MIGRATOR: &str = "application.verification_migrator";
const APPLICATION_UPGRADE_STEP: &str = "application.upgrade_step";
const APPLICATION_STATUS: &str = "application.status";
const APPLICATION_START_PROJECTOR: &str = "application.projector.start";
const APPLICATION_PROJECT: &str = "application.projector.project";
const CLIENT_DESCRIBE: &str = "client.describe";
const CLIENT_TRANSIENT_BODY: &str = "client.transient_body";
const CLIENT_PARSE_MCP: &str = "client.parse_mcp";
const CLIENT_PARSE_WEB: &str = "client.parse_web";
const CLIENT_CLASSIFY_FAILURE: &str = "client.classify_failure";
const CLIENT_CONFIRMATION: &str = "client.confirmation";
const CLIENT_EXECUTE: &str = "client.execute";
const CLIENT_DISPLAY_CANONICALIZE: &str = "client.display.canonicalize";
const CLIENT_DISPLAY_PREPARE: &str = "client.display.prepare";
const CLIENT_DISPLAY_PROJECT: &str = "client.display.project";
const CLIENT_DISPLAY_CHOICES_PREPARE: &str = "client.display.choices.prepare";
const CLIENT_DISPLAY_CHOICES_PROJECT: &str = "client.display.choices.project";
const EXEC_DESCRIBE: &str = "exec.describe";
const EXEC_HANDLE: &str = "exec.handle";
const EXEC_FIND: &str = "exec.find";
const EXEC_READ_CONTENT: &str = "exec.read_content";
const EXEC_CANCEL_ASKED: &str = "exec.cancel_asked";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientDeclaration {
    pub mount: String,
    pub tools: Vec<ClientToolDeclaration>,
    pub instructions: String,
    pub without: Vec<String>,
    pub display: ClientDisplayDeclaration,
    pub display_surfaces: Vec<world_interface::display::DisplaySurfaceDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientToolDeclaration {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientDisplayDeclaration {
    pub name: String,
    pub icon: Option<String>,
    pub entry_path: Option<String>,
    pub tagline: Option<String>,
    pub accent: Option<u32>,
    pub routes: Vec<(String, String)>,
    pub mark: Option<Vec<u8>>,
    pub hero: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ClientOrigin {
    Mcp {
        tool: String,
        input: serde_json::Value,
    },
    Web(serde_json::Value),
    Display(world_interface::display::DisplayRequest),
    /// The listing of what a display surface can show.
    DisplayChoices(world_interface::display::DisplaySurfaceId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParsedClientInvocation {
    access: world_interface::ClientAccess,
    confirmation_question: Option<String>,
    origin: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientInvocationRequest {
    origin: Vec<u8>,
    local_root: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DisplayProjectRequest {
    value: serde_json::Value,
    request: world_interface::display::DisplayRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecDeclaration {
    builds: Vec<runtime::exec::Build>,
    handlers: Vec<runtime::exec::HandlerBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecContextSeed {
    handler: runtime::exec::HandlerBinding,
    resume_checkpoint: Option<runtime::exec::CheckpointRef>,
    committed_checkpoint_count: u32,
    world: WorldId,
    run: runtime::exec::RunId,
    attempt: runtime::exec::AttemptId,
    spec: runtime::exec::SchemaRef,
    build: runtime::exec::BuildId,
    input_schema: runtime::exec::SchemaRef,
    input_inline: Vec<u8>,
    input_content: Vec<replica::content::ContentRef>,
    accepted_resources: Vec<runtime::exec::Resource>,
    enforcement_evidence: Option<replica::content::ContentRef>,
    limits: runtime::exec::AttemptLimits,
    links: Vec<runtime::exec::LinkSpec>,
    cancel_asked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ExecStaging {
    SaveCheckpoint(runtime::exec::CheckpointRef),
    SaveCheckpointBytes(Vec<u8>),
    StartChild(runtime::exec::Start),
    StageOutput(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecCompletion {
    candidate: runtime::exec::Candidate,
    staging: Vec<ExecStaging>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum ExecFailure {
    Cancelled,
    Handler,
    Wall,
    Os,
    InvalidContext,
    InvalidOutcome,
    InvalidCheckpoint,
    CheckpointLimit,
    InvalidChild,
    ChildLimit,
    QueryUnavailable,
    QueryRefused,
    QueryBudget,
    ContentUnavailable,
    ContentRefused,
}

impl From<runtime::exec::Failure> for ExecFailure {
    fn from(value: runtime::exec::Failure) -> Self {
        match value {
            runtime::exec::Failure::Cancelled => Self::Cancelled,
            runtime::exec::Failure::Handler => Self::Handler,
            runtime::exec::Failure::Wall => Self::Wall,
            runtime::exec::Failure::Os => Self::Os,
            runtime::exec::Failure::InvalidContext => Self::InvalidContext,
            runtime::exec::Failure::InvalidOutcome => Self::InvalidOutcome,
            runtime::exec::Failure::InvalidCheckpoint => Self::InvalidCheckpoint,
            runtime::exec::Failure::CheckpointLimit => Self::CheckpointLimit,
            runtime::exec::Failure::InvalidChild => Self::InvalidChild,
            runtime::exec::Failure::ChildLimit => Self::ChildLimit,
            runtime::exec::Failure::QueryUnavailable => Self::QueryUnavailable,
            runtime::exec::Failure::QueryRefused(_) => Self::QueryRefused,
            runtime::exec::Failure::QueryBudget => Self::QueryBudget,
            runtime::exec::Failure::ContentUnavailable => Self::ContentUnavailable,
            runtime::exec::Failure::ContentRefused => Self::ContentRefused,
        }
    }
}

impl ExecFailure {
    fn runtime(self) -> runtime::exec::Failure {
        match self {
            Self::Cancelled => runtime::exec::Failure::Cancelled,
            Self::Handler | Self::QueryRefused => runtime::exec::Failure::Handler,
            Self::Wall => runtime::exec::Failure::Wall,
            Self::Os => runtime::exec::Failure::Os,
            Self::InvalidContext => runtime::exec::Failure::InvalidContext,
            Self::InvalidOutcome => runtime::exec::Failure::InvalidOutcome,
            Self::InvalidCheckpoint => runtime::exec::Failure::InvalidCheckpoint,
            Self::CheckpointLimit => runtime::exec::Failure::CheckpointLimit,
            Self::InvalidChild => runtime::exec::Failure::InvalidChild,
            Self::ChildLimit => runtime::exec::Failure::ChildLimit,
            Self::QueryUnavailable => runtime::exec::Failure::QueryUnavailable,
            Self::QueryBudget => runtime::exec::Failure::QueryBudget,
            Self::ContentUnavailable => runtime::exec::Failure::ContentUnavailable,
            Self::ContentRefused => runtime::exec::Failure::ContentRefused,
        }
    }
}

/// One founder capability declared by an independently shipped World.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FounderGrant {
    pub capability: mechanics::authorization::PolicyCapability,
    pub resource: mechanics::authorization::Resource,
    pub salt: [u8; 16],
}

/// A World-owned container created with a new Space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialScope {
    pub kind: String,
    pub key: String,
    pub name: String,
}

/// An exact reviewed implementation coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedImplementation {
    pub id: [u8; 32],
    pub version: u32,
}

/// A World's pure decision about changing its active implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorldUpgradeAssessment {
    Current,
    Direct,
    ConsentRequired { migrator: ReviewedImplementation },
    InProgress { migrator: ReviewedImplementation },
    Unsupported { reason: String },
}

/// Result of one bounded and crash-idempotent World migration step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorldUpgradeProgress {
    Pending {
        completed: u64,
        remaining: Option<u64>,
        record: Vec<u8>,
    },
    Verified {
        record: Vec<u8>,
    },
}

/// Product-neutral status material projected by a World.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusProjection {
    pub items: usize,
    pub scopes: usize,
    pub name: String,
    pub description: String,
}

/// Resources supplied to a World's formation hook. Storage and authority stay
/// with the host; the World sees only its private checkpoint root and bounded
/// Runtime capabilities.
pub struct BootstrapContext<'a> {
    pub store_root: &'a Path,
    pub space: &'a mechanics::ids::SpaceId,
    pub session: &'a dyn SessionAccess,
    pub identity: &'a dyn IdentityAccess,
    pub device: &'a str,
    pub display_name: &'a str,
    pub initial_scope: Option<&'a InitialScope>,
}

/// Resources supplied to one bounded lifecycle migration step.
pub struct WorldUpgradeContext<'a> {
    pub space: &'a mechanics::ids::SpaceId,
    pub session: &'a dyn SessionAccess,
    pub identity: &'a dyn IdentityAccess,
    pub device: &'a str,
    pub active: ReviewedImplementation,
    pub migrator: ReviewedImplementation,
    pub preferred: ReviewedImplementation,
    pub source: &'a LifecycleSourceCoordinate,
    pub record: Option<&'a [u8]>,
}

/// The complete application-side behavior owned by one World release.
///
/// The trait deliberately uses Runtime's capability interfaces rather than
/// concrete host types. The same implementation therefore runs in a child
/// process while the client keeps Sessions, keys, persistence, and authority.
pub trait WorldApplication: Send + Sync {
    fn founder_grants(&self) -> anyhow::Result<Vec<FounderGrant>> {
        Ok(Vec::new())
    }

    /// Expand one World-owned role selector into exact generic authority.
    ///
    /// `None` means this World does not define admission roles. The host signs
    /// and validates the returned evidence; the World never receives signing
    /// keys or mutates membership authority.
    fn admission_evidence(
        &self,
        _role: &str,
        _parent_manifest_root: [u8; 32],
    ) -> anyhow::Result<Option<mechanics::authorization::WorldAssignmentEvidence>> {
        Ok(None)
    }

    fn initial_scope(&self, _display_name: &str) -> Option<InitialScope> {
        None
    }

    fn bootstrap(&self, _context: BootstrapContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    fn assess_upgrade(
        &self,
        active: Option<ReviewedImplementation>,
        preferred: ReviewedImplementation,
    ) -> anyhow::Result<WorldUpgradeAssessment> {
        Ok(if active.is_some_and(|active| active.id == preferred.id) {
            WorldUpgradeAssessment::Current
        } else {
            WorldUpgradeAssessment::Direct
        })
    }

    fn verification_migrator(
        &self,
        _preferred: ReviewedImplementation,
    ) -> Option<ReviewedImplementation> {
        None
    }

    fn upgrade_step(
        &self,
        _context: WorldUpgradeContext<'_>,
    ) -> anyhow::Result<WorldUpgradeProgress> {
        anyhow::bail!("this World has no lifecycle upgrade step")
    }

    fn status(&self, _session: &dyn SessionAccess) -> Option<StatusProjection> {
        None
    }

    fn start_projector(&self, _session: &dyn SessionAccess, _space: &mechanics::ids::SpaceId) {}

    fn project(
        &self,
        _session: &dyn SessionAccess,
        _space: &mechanics::ids::SpaceId,
        _observation: &runtime::world::Observation,
    ) -> runtime::world::Invalidation {
        runtime::world::Invalidation::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContextSeed {
    principal: PrincipalFacts,
    manifest_root: [u8; 32],
    publication: Option<runtime::publication::WorldPublicationId>,
    request: Option<runtime::world::RequestId>,
    lifecycle_source: Option<LifecycleSourceCoordinate>,
    has_reads: bool,
    has_find: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubmitRequest {
    context: ContextSeed,
    intent: Intent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryRequest {
    context: ContextSeed,
    query: Query,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExtractRequest {
    publication: runtime::publication::WorldPublicationId,
    extractor: runtime::find::Extractor,
    body: BodyKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApplicationContextSeed {
    principal: PrincipalFacts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApplicationCallRequest {
    context: ApplicationContextSeed,
    call: ApplicationCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApplicationNudgesRequest {
    context: ApplicationContextSeed,
    call: ApplicationCall,
    reply: ApplicationReply,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BootstrapRequest {
    context: ApplicationContextSeed,
    store_root: std::path::PathBuf,
    space: mechanics::ids::SpaceId,
    device: String,
    display_name: String,
    initial_scope: Option<InitialScope>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpgradeStepRequest {
    context: ApplicationContextSeed,
    space: mechanics::ids::SpaceId,
    device: String,
    active: ReviewedImplementation,
    migrator: ReviewedImplementation,
    preferred: ReviewedImplementation,
    source: LifecycleSourceCoordinate,
    record: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectorRequest {
    context: ApplicationContextSeed,
    space: mechanics::ids::SpaceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectRequest {
    context: ApplicationContextSeed,
    space: mechanics::ids::SpaceId,
    observation: runtime::world::Observation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryAtRequest {
    publication: runtime::publication::WorldPublicationId,
    query: runtime::world::Query,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FindAtRequest {
    publication: runtime::publication::WorldPublicationId,
    query: runtime::find::Query,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignRequest {
    request: runtime::world::RequestId,
    intent: runtime::world::Intent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LifecycleReadRequest<T> {
    source: LifecycleSourceCoordinate,
    input: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchemaCoordinate {
    world: WorldId,
    schema: SchemaId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageRequest {
    world: WorldId,
    schema: SchemaId,
    after: Option<BodyKey>,
    limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnchorRequest {
    key: BodyKey,
    path: String,
    position: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResolveAnchorRequest {
    key: BodyKey,
    anchor: fabric::Anchor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutcomeRequest {
    world: WorldId,
    run: runtime::exec::RunId,
    attempt: runtime::exec::AttemptId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenQuery {
    token: u64,
    query: runtime::find::Query,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenBytes {
    token: u64,
    bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum FindFailure {
    Invalid(String),
    Interrupted,
    PrincipalDenied,
    NoActiveImplementation,
    ImplementationUnavailable,
    AuthorityUnavailable(String),
    PolicyExceeded,
    PublicationUnavailable,
    PublicationExpired,
    PaginationUnsupported,
    CursorCapacityExceeded,
    Unavailable,
}

impl From<runtime::find::Failure> for FindFailure {
    fn from(value: runtime::find::Failure) -> Self {
        use runtime::find::Failure;
        match value {
            Failure::Invalid(reason) => Self::Invalid(reason.to_string()),
            Failure::Interrupted => Self::Interrupted,
            Failure::PrincipalDenied => Self::PrincipalDenied,
            Failure::NoActiveImplementation => Self::NoActiveImplementation,
            Failure::ImplementationUnavailable => Self::ImplementationUnavailable,
            Failure::AuthorityUnavailable(reason) => Self::AuthorityUnavailable(reason),
            Failure::PolicyExceeded => Self::PolicyExceeded,
            Failure::PublicationUnavailable => Self::PublicationUnavailable,
            Failure::PublicationExpired => Self::PublicationExpired,
            Failure::PaginationUnsupported => Self::PaginationUnsupported,
            Failure::CursorCapacityExceeded => Self::CursorCapacityExceeded,
            Failure::Unavailable => Self::Unavailable,
        }
    }
}

impl From<FindFailure> for runtime::find::Failure {
    fn from(value: FindFailure) -> Self {
        use runtime::find::Failure;
        match value {
            // Invalid input was already validated before leaving the child;
            // the host's detailed static reason cannot safely be reconstructed.
            FindFailure::Invalid(_) => Failure::Unavailable,
            FindFailure::Interrupted => Failure::Interrupted,
            FindFailure::PrincipalDenied => Failure::PrincipalDenied,
            FindFailure::NoActiveImplementation => Failure::NoActiveImplementation,
            FindFailure::ImplementationUnavailable => Failure::ImplementationUnavailable,
            FindFailure::AuthorityUnavailable(reason) => Failure::AuthorityUnavailable(reason),
            FindFailure::PolicyExceeded => Failure::PolicyExceeded,
            FindFailure::PublicationUnavailable => Failure::PublicationUnavailable,
            FindFailure::PublicationExpired => Failure::PublicationExpired,
            FindFailure::PaginationUnsupported => Failure::PaginationUnsupported,
            FindFailure::CursorCapacityExceeded => Failure::CursorCapacityExceeded,
            FindFailure::Unavailable => Failure::Unavailable,
        }
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    // Normalize through Serde's JSON data model before writing CBOR. Runtime
    // DTOs deliberately use JSON-compatible tagged/flattened representations,
    // and process packages also carry arbitrary `serde_json::Value`. Feeding
    // those types straight to a non-self-describing deserializer loses the map
    // shape of flattened enums (for example World-call `Reply::status`).
    let value = serde_json::to_value(value)
        .map_err(|error| format!("normalize World runtime message: {error}"))?;
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes)
        .map_err(|error| format!("encode World runtime message: {error}"))?;
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let value: serde_json::Value = ciborium::de::from_reader(bytes)
        .map_err(|error| format!("decode World runtime message: {error}"))?;
    let shape = match &value {
        serde_json::Value::Object(fields) => format!(
            "object with keys {:?}",
            fields.keys().map(String::as_str).collect::<Vec<_>>()
        ),
        serde_json::Value::Array(values) => format!("array of length {}", values.len()),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(_) => "boolean".to_string(),
        serde_json::Value::Number(_) => "number".to_string(),
        serde_json::Value::String(_) => "string".to_string(),
    };
    serde_json::from_value(value).map_err(|error| {
        format!(
            "materialize World runtime message as {} from {shape}: {error}",
            std::any::type_name::<T>()
        )
    })
}

fn host_call<I: Serialize, O: DeserializeOwned>(
    host: &dyn Host,
    operation: &str,
    input: &I,
) -> Result<O, String> {
    let request = encode(input).map_err(|error| format!("{operation}: {error}"))?;
    let response = host
        .call(operation, &request)
        .map_err(|error| format!("{operation}: {error}"))?;
    decode(&response).map_err(|error| format!("{operation}: {error}"))
}

/// Adapts a product's ordinary Runtime implementation to the runner service.
pub struct WorldService<W> {
    world: W,
    reviewed: [u8; 32],
    handler: Option<Arc<dyn Handler>>,
    application: Option<Arc<dyn WorldApplication>>,
    client: Option<world_interface::WorldClientPackage>,
    exec: runtime::exec::Package,
}

impl<W> WorldService<W> {
    pub fn new(world: W, reviewed: [u8; 32]) -> Self {
        Self {
            world,
            reviewed,
            handler: None,
            application: None,
            client: None,
            exec: runtime::exec::Package::new(),
        }
    }

    /// Add the product's application router to the same exact implementation
    /// process. Runtime still owns its Session and signing identity; the child
    /// receives only the bounded [`ApplicationContext`] facade.
    pub fn with_handler(mut self, handler: Arc<dyn Handler>) -> Self {
        self.handler = Some(handler);
        self
    }

    /// Add formation, lifecycle, projection, and status behavior owned by this
    /// exact World release.
    pub fn with_application(mut self, application: Arc<dyn WorldApplication>) -> Self {
        self.application = Some(application);
        self
    }

    /// Add the HTTP, MCP, and display package declared by this release.
    pub fn with_client(mut self, client: world_interface::WorldClientPackage) -> Self {
        self.client = Some(client);
        self
    }

    /// Add the exact executable Builds implemented by this release.
    pub fn with_exec(mut self, exec: runtime::exec::Package) -> Self {
        self.exec = exec;
        self
    }
}

impl<W: World> Service for WorldService<W> {
    fn descriptor(&self) -> ServiceDescriptor {
        let descriptor = self.world.descriptor();
        ServiceDescriptor {
            world: descriptor.id.to_string(),
            implementation: self.reviewed,
            implementation_version: descriptor.implementation_version.0,
        }
    }

    fn call(
        &self,
        operation: &str,
        payload: &[u8],
        host: Arc<dyn Host>,
    ) -> Result<Vec<u8>, String> {
        match operation {
            EXEC_DESCRIBE => {
                let _: () = decode(payload)?;
                encode(&ExecDeclaration {
                    builds: self.exec.builds().to_vec(),
                    handlers: self
                        .exec
                        .handlers()
                        .iter()
                        .map(|handler| handler.binding().clone())
                        .collect(),
                })
            }
            EXEC_HANDLE => {
                let seed: ExecContextSeed = decode(payload)?;
                let handler = self
                    .exec
                    .handlers()
                    .iter()
                    .find(|handler| handler.binding() == &seed.handler)
                    .ok_or_else(|| {
                        "this World does not implement the selected Exec Build".to_string()
                    })?;
                let mut context = RemoteExecContext::new(seed, host);
                let result = handler
                    .handle(&mut context)
                    .map(|candidate| ExecCompletion {
                        candidate,
                        staging: context.staging,
                    })
                    .map_err(ExecFailure::from);
                encode(&result)
            }
            CLIENT_DESCRIBE => {
                let _: () = decode(payload)?;
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                encode(&client_declaration(client))
            }
            CLIENT_TRANSIENT_BODY => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let document: String = decode(payload)?;
                encode(&client.transient_body(&document))
            }
            CLIENT_PARSE_MCP | CLIENT_PARSE_WEB => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let origin = if operation == CLIENT_PARSE_MCP {
                    let (tool, input) = decode(payload)?;
                    ClientOrigin::Mcp { tool, input }
                } else {
                    ClientOrigin::Web(decode(payload)?)
                };
                let encoded_origin = encode(&origin)?;
                let result = parse_client_origin(client, &origin).and_then(|invocation| {
                    client.validate_invocation(&invocation)?;
                    Ok(ParsedClientInvocation {
                        access: invocation.access(),
                        confirmation_question: invocation
                            .confirmation_question()
                            .map(str::to_string),
                        origin: encoded_origin,
                    })
                });
                encode(&result)
            }
            CLIENT_CLASSIFY_FAILURE => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let value = decode(payload)?;
                encode(&client.classify_failure(&value))
            }
            CLIENT_DISPLAY_CANONICALIZE => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let (surface_id, value): (
                    world_interface::display::DisplaySurfaceId,
                    serde_json::Value,
                ) = decode(payload)?;
                let result = client
                    .display_surface(&surface_id)
                    .ok_or_else(|| {
                        world_interface::Failure::new(format!(
                            "unknown display surface {}",
                            surface_id.as_str()
                        ))
                    })
                    .and_then(|surface| surface.canonicalize_input(value));
                encode(&result)
            }
            CLIENT_DISPLAY_PREPARE => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let request: world_interface::display::DisplayRequest = decode(payload)?;
                let origin = ClientOrigin::Display(request);
                let encoded_origin = encode(&origin)?;
                let result = parse_client_origin(client, &origin).and_then(|invocation| {
                    client.validate_invocation(&invocation)?;
                    Ok(ParsedClientInvocation {
                        access: invocation.access(),
                        confirmation_question: invocation
                            .confirmation_question()
                            .map(str::to_string),
                        origin: encoded_origin,
                    })
                });
                encode(&result)
            }
            CLIENT_DISPLAY_PROJECT => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let project: DisplayProjectRequest = decode(payload)?;
                let result = client
                    .display_surface(&project.request.surface)
                    .ok_or_else(|| {
                        world_interface::Failure::new(format!(
                            "unknown display surface {}",
                            project.request.surface.as_str()
                        ))
                    })
                    .and_then(|surface| {
                        futures_lite::future::block_on(
                            surface.project(project.value, &project.request),
                        )
                    });
                encode(&result)
            }
            CLIENT_DISPLAY_CHOICES_PREPARE => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let surface_id: world_interface::display::DisplaySurfaceId = decode(payload)?;
                let origin = ClientOrigin::DisplayChoices(surface_id.clone());
                let encoded_origin = encode(&origin)?;
                // A surface that lists nothing is `Ok(None)`, told apart from
                // a listing that failed to prepare.
                let result = client
                    .display_surface(&surface_id)
                    .ok_or_else(|| {
                        world_interface::Failure::new(format!(
                            "unknown display surface {}",
                            surface_id.as_str()
                        ))
                    })
                    .and_then(|surface| surface.choices_prepare())
                    .and_then(|prepared| {
                        prepared
                            .map(|invocation| {
                                client.validate_invocation(&invocation)?;
                                Ok(ParsedClientInvocation {
                                    access: invocation.access(),
                                    confirmation_question: invocation
                                        .confirmation_question()
                                        .map(str::to_string),
                                    origin: encoded_origin.clone(),
                                })
                            })
                            .transpose()
                    });
                encode(&result)
            }
            CLIENT_DISPLAY_CHOICES_PROJECT => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let (surface_id, value): (
                    world_interface::display::DisplaySurfaceId,
                    serde_json::Value,
                ) = decode(payload)?;
                let result = client
                    .display_surface(&surface_id)
                    .ok_or_else(|| {
                        world_interface::Failure::new(format!(
                            "unknown display surface {}",
                            surface_id.as_str()
                        ))
                    })
                    .and_then(|surface| surface.choices_project(value));
                encode(&result)
            }
            CLIENT_CONFIRMATION | CLIENT_EXECUTE => {
                let client = self
                    .client
                    .as_ref()
                    .ok_or_else(|| "this World exposes no client package".to_string())?;
                let request: ClientInvocationRequest = decode(payload)?;
                let origin: ClientOrigin = decode(&request.origin)?;
                let invocation = match parse_client_origin(client, &origin) {
                    Ok(invocation) => invocation,
                    Err(error) => {
                        return if operation == CLIENT_CONFIRMATION {
                            encode(&Err::<Option<String>, _>(error))
                        } else {
                            encode(&Err::<serde_json::Value, _>(error))
                        };
                    }
                };
                let remote_host = RemoteClientHost {
                    host,
                    local_root: request.local_root,
                };
                if operation == CLIENT_CONFIRMATION {
                    let result = futures_lite::future::block_on(
                        client.confirmation(&remote_host, &invocation),
                    );
                    encode(&result)
                } else {
                    let result =
                        futures_lite::future::block_on(client.execute(&remote_host, invocation));
                    encode(&result)
                }
            }
            APPLICATION_FOUNDER_GRANTS => {
                let _: () = decode(payload)?;
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application lifecycle".to_string())?;
                encode(
                    &application
                        .founder_grants()
                        .map_err(|error| format!("{error:#}")),
                )
            }
            APPLICATION_ADMISSION_EVIDENCE => {
                let (role, parent_manifest_root): (String, [u8; 32]) = decode(payload)?;
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application lifecycle".to_string())?;
                encode(
                    &application
                        .admission_evidence(&role, parent_manifest_root)
                        .map_err(|error| format!("{error:#}")),
                )
            }
            APPLICATION_INITIAL_SCOPE => {
                let display_name: String = decode(payload)?;
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application lifecycle".to_string())?;
                encode(&application.initial_scope(&display_name))
            }
            APPLICATION_ASSESS_UPGRADE => {
                let (active, preferred) = decode(payload)?;
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application lifecycle".to_string())?;
                encode(
                    &application
                        .assess_upgrade(active, preferred)
                        .map_err(|error| format!("{error:#}")),
                )
            }
            APPLICATION_VERIFICATION_MIGRATOR => {
                let preferred = decode(payload)?;
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application lifecycle".to_string())?;
                encode(&application.verification_migrator(preferred))
            }
            APPLICATION_BOOTSTRAP => {
                let request: BootstrapRequest = decode(payload)?;
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application lifecycle".to_string())?;
                let session = RemoteApplicationSession::new(
                    Arc::clone(&host),
                    request.context.principal.clone(),
                    self.world.id(),
                );
                let identity = RemoteApplicationIdentity::new(
                    Arc::clone(&host),
                    request.context.principal.device.clone(),
                );
                encode(
                    &application
                        .bootstrap(BootstrapContext {
                            store_root: &request.store_root,
                            space: &request.space,
                            session: &session,
                            identity: &identity,
                            device: &request.device,
                            display_name: &request.display_name,
                            initial_scope: request.initial_scope.as_ref(),
                        })
                        .map_err(|error| format!("{error:#}")),
                )
            }
            APPLICATION_UPGRADE_STEP => {
                let request: UpgradeStepRequest = decode(payload)?;
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application lifecycle".to_string())?;
                let session = RemoteApplicationSession::new(
                    Arc::clone(&host),
                    request.context.principal.clone(),
                    self.world.id(),
                );
                let identity = RemoteApplicationIdentity::new(
                    Arc::clone(&host),
                    request.context.principal.device.clone(),
                );
                encode(
                    &application
                        .upgrade_step(WorldUpgradeContext {
                            space: &request.space,
                            session: &session,
                            identity: &identity,
                            device: &request.device,
                            active: request.active,
                            migrator: request.migrator,
                            preferred: request.preferred,
                            source: &request.source,
                            record: request.record.as_deref(),
                        })
                        .map_err(|error| format!("{error:#}")),
                )
            }
            APPLICATION_STATUS | APPLICATION_START_PROJECTOR | APPLICATION_PROJECT => {
                let application = self
                    .application
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application projector".to_string())?;
                if operation == APPLICATION_STATUS {
                    let request: ApplicationContextSeed = decode(payload)?;
                    let session = RemoteApplicationSession::new(
                        Arc::clone(&host),
                        request.principal,
                        self.world.id(),
                    );
                    encode(&application.status(&session))
                } else if operation == APPLICATION_START_PROJECTOR {
                    let request: ProjectorRequest = decode(payload)?;
                    let session = RemoteApplicationSession::new(
                        Arc::clone(&host),
                        request.context.principal,
                        self.world.id(),
                    );
                    application.start_projector(&session, &request.space);
                    encode(&())
                } else {
                    let request: ProjectRequest = decode(payload)?;
                    let session = RemoteApplicationSession::new(
                        Arc::clone(&host),
                        request.context.principal,
                        self.world.id(),
                    );
                    encode(&application.project(&session, &request.space, &request.observation))
                }
            }
            APPLICATION_ACCESS => {
                let call: ApplicationCall = decode(payload)?;
                let handler = self
                    .handler
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application handler".to_string())?;
                encode(&handler.access(&call))
            }
            APPLICATION_CALL | APPLICATION_NUDGES => {
                let handler = self
                    .handler
                    .as_deref()
                    .ok_or_else(|| "this World exposes no application handler".to_string())?;
                if operation == APPLICATION_CALL {
                    let request: ApplicationCallRequest = decode(payload)?;
                    let session = RemoteApplicationSession::new(
                        Arc::clone(&host),
                        request.context.principal.clone(),
                        self.world.id(),
                    );
                    let identity = RemoteApplicationIdentity::new(
                        Arc::clone(&host),
                        request.context.principal.device.clone(),
                    );
                    let actor = request.context.principal.actor.to_string();
                    let device = request.context.principal.device.to_string();
                    let context = ApplicationContext {
                        session: &session,
                        identity: &identity,
                        actor: &actor,
                        device: &device,
                    };
                    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler.call(&request.call, &context)
                    }))
                    .unwrap_or_else(|_| {
                        ApplicationReply::error(
                            &request.call,
                            runtime::world::call::Code::Internal,
                            "World application handler panicked",
                        )
                    });
                    encode(&reply)
                } else {
                    let request: ApplicationNudgesRequest = decode(payload)?;
                    let session = RemoteApplicationSession::new(
                        Arc::clone(&host),
                        request.context.principal.clone(),
                        self.world.id(),
                    );
                    let identity = RemoteApplicationIdentity::new(
                        Arc::clone(&host),
                        request.context.principal.device.clone(),
                    );
                    let actor = request.context.principal.actor.to_string();
                    let device = request.context.principal.device.to_string();
                    let context = ApplicationContext {
                        session: &session,
                        identity: &identity,
                        actor: &actor,
                        device: &device,
                    };
                    let nudges = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler.nudges(&request.call, &request.reply, &context)
                    }))
                    .unwrap_or_default();
                    encode(&nudges)
                }
            }
            DESCRIBE => encode(&self.world.descriptor()),
            SUBMIT => {
                let request: SubmitRequest = decode(payload)?;
                let fault = Arc::new(Mutex::new(None));
                let reads = RemoteReader::new(Arc::clone(&host), false, Arc::clone(&fault));
                let lifecycle = RemoteReader::new(Arc::clone(&host), true, Arc::clone(&fault));
                let find = request
                    .context
                    .has_find
                    .then(|| {
                        request.context.publication.map(|publication| {
                            FindHandle::hosted(Arc::new(RemoteFindReader::new(
                                Arc::clone(&host),
                                publication,
                                Arc::clone(&fault),
                            )))
                        })
                    })
                    .flatten();
                let world = self.world.id();
                let mut context = Context::from_runner(
                    &request.context.principal,
                    request.context.has_reads.then_some(&reads),
                    request
                        .context
                        .lifecycle_source
                        .as_ref()
                        .map(|_| &lifecycle as &dyn BodyReader),
                    Some(&world),
                    request.context.request,
                    request.context.manifest_root,
                    request.context.publication,
                    find,
                    request.context.lifecycle_source,
                );
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.world.submit(&mut context, request.intent)
                }))
                .unwrap_or(Err(Rejection::ContractViolation));
                encode(&faulted(outcome, &fault))
            }
            QUERY => {
                let request: QueryRequest = decode(payload)?;
                let fault = Arc::new(Mutex::new(None));
                let reads = RemoteReader::new(Arc::clone(&host), false, Arc::clone(&fault));
                let find = request
                    .context
                    .has_find
                    .then(|| {
                        request.context.publication.map(|publication| {
                            FindHandle::hosted(Arc::new(RemoteFindReader::new(
                                Arc::clone(&host),
                                publication,
                                Arc::clone(&fault),
                            )))
                        })
                    })
                    .flatten();
                let world = self.world.id();
                let context = Context::from_runner(
                    &request.context.principal,
                    request.context.has_reads.then_some(&reads),
                    None,
                    Some(&world),
                    request.context.request,
                    request.context.manifest_root,
                    request.context.publication,
                    find,
                    request.context.lifecycle_source,
                );
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.world.query(&context, request.query)
                }))
                .unwrap_or(Err(Rejection::ContractViolation));
                encode(&faulted(outcome, &fault))
            }
            EXTRACT => {
                let request: ExtractRequest = decode(payload)?;
                let fault = Arc::new(Mutex::new(None));
                let reads = RemoteReader::new(host, false, Arc::clone(&fault));
                let world = self.world.id();
                let context = ExtractionContext::from_runner(&reads, &world, request.publication);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.world
                        .extract(&context, &request.extractor, &request.body)
                }))
                .unwrap_or(Err(Rejection::ContractViolation));
                encode(&faulted(outcome, &fault))
            }
            _ => Err(format!("unsupported semantic World operation {operation}")),
        }
    }
}

fn client_declaration(client: &world_interface::WorldClientPackage) -> ClientDeclaration {
    let display = client.display();
    ClientDeclaration {
        mount: client.mount().to_string(),
        tools: client
            .mcp_tools()
            .iter()
            .map(|tool| ClientToolDeclaration {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                schema: tool.schema(),
            })
            .collect(),
        instructions: client.mcp_instructions().to_string(),
        without: client
            .without()
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        display: ClientDisplayDeclaration {
            name: display.name().to_string(),
            icon: display.icon().map(str::to_string),
            entry_path: display.entry_path().map(str::to_string),
            tagline: display.tagline().map(str::to_string),
            accent: display.accent(),
            routes: display
                .routes()
                .iter()
                .map(|route| (route.label().to_string(), route.path().to_string()))
                .collect(),
            mark: display.mark().map(<[u8]>::to_vec),
            hero: display.hero().map(<[u8]>::to_vec),
        },
        display_surfaces: client
            .display_surfaces()
            .map(|surface| surface.descriptor.clone())
            .collect(),
    }
}

fn parse_client_origin(
    client: &world_interface::WorldClientPackage,
    origin: &ClientOrigin,
) -> Result<world_interface::ClientInvocation, world_interface::Failure> {
    match origin {
        ClientOrigin::Mcp { tool, input } => client
            .mcp_tools()
            .iter()
            .find(|candidate| candidate.name() == tool)
            .ok_or_else(|| world_interface::Failure::new(format!("unknown MCP tool {tool}")))?
            .call(input.clone()),
        ClientOrigin::Web(input) => client.parse_web(input.clone()),
        ClientOrigin::Display(request) => client
            .display_surface(&request.surface)
            .ok_or_else(|| {
                world_interface::Failure::new(format!(
                    "unknown display surface {}",
                    request.surface.as_str()
                ))
            })?
            .prepare(request),
        ClientOrigin::DisplayChoices(surface) => client
            .display_surface(surface)
            .ok_or_else(|| {
                world_interface::Failure::new(format!(
                    "unknown display surface {}",
                    surface.as_str()
                ))
            })?
            .choices_prepare()?
            .ok_or_else(|| {
                world_interface::Failure::new(format!(
                    "display surface {} lists no choices",
                    surface.as_str()
                ))
            }),
    }
}

struct RemoteExecContext {
    seed: ExecContextSeed,
    host: Arc<dyn Host>,
    staging: Vec<ExecStaging>,
    staged_checkpoints: u32,
    staged_checkpoint_bytes: u64,
    staged_children: u32,
    staged_outputs: u32,
}

impl RemoteExecContext {
    fn new(seed: ExecContextSeed, host: Arc<dyn Host>) -> Self {
        Self {
            seed,
            host,
            staging: Vec::new(),
            staged_checkpoints: 0,
            staged_checkpoint_bytes: 0,
            staged_children: 0,
            staged_outputs: 0,
        }
    }

    fn next_checkpoint(&self) -> Result<u32, runtime::exec::Failure> {
        self.seed
            .committed_checkpoint_count
            .checked_add(self.staged_checkpoints)
            .and_then(|count| count.checked_add(1))
            .ok_or(runtime::exec::Failure::CheckpointLimit)
    }
}

impl runtime::exec::HandlerContext for RemoteExecContext {
    fn resume_checkpoint(&self) -> Option<&runtime::exec::CheckpointRef> {
        self.seed.resume_checkpoint.as_ref()
    }

    fn read_content(
        &self,
        content: &replica::content::ContentRef,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, runtime::exec::Failure> {
        host_call::<_, Result<Vec<u8>, ExecFailure>>(
            self.host.as_ref(),
            EXEC_READ_CONTENT,
            &(*content, offset, len),
        )
        .map_err(|_| runtime::exec::Failure::Handler)?
        .map_err(ExecFailure::runtime)
    }

    fn query(
        &mut self,
        query: runtime::find::Query,
    ) -> Result<runtime::find::Answer, runtime::exec::Failure> {
        host_call::<_, Result<runtime::find::Answer, ExecFailure>>(
            self.host.as_ref(),
            EXEC_FIND,
            &query,
        )
        .map_err(|_| runtime::exec::Failure::Handler)?
        .map_err(ExecFailure::runtime)
    }

    fn world(&self) -> &WorldId {
        &self.seed.world
    }

    fn run(&self) -> runtime::exec::RunId {
        self.seed.run
    }

    fn attempt(&self) -> runtime::exec::AttemptId {
        self.seed.attempt
    }

    fn spec(&self) -> &runtime::exec::SchemaRef {
        &self.seed.spec
    }

    fn build(&self) -> runtime::exec::BuildId {
        self.seed.build
    }

    fn input_schema(&self) -> &runtime::exec::SchemaRef {
        &self.seed.input_schema
    }

    fn input_inline(&self) -> &[u8] {
        &self.seed.input_inline
    }

    fn input_content(&self) -> &[replica::content::ContentRef] {
        &self.seed.input_content
    }

    fn accepted_resources(&self) -> &[runtime::exec::Resource] {
        &self.seed.accepted_resources
    }

    fn enforcement_evidence(&self) -> Option<replica::content::ContentRef> {
        self.seed.enforcement_evidence
    }

    fn limits(&self) -> runtime::exec::AttemptLimits {
        self.seed.limits
    }

    fn links(&self) -> &[runtime::exec::LinkSpec] {
        &self.seed.links
    }

    fn cancel_asked(&self) -> bool {
        if self.seed.cancel_asked {
            return true;
        }
        host_call::<_, bool>(self.host.as_ref(), EXEC_CANCEL_ASKED, &()).unwrap_or(true)
    }

    fn committed_checkpoint_count(&self) -> u32 {
        self.seed.committed_checkpoint_count
    }

    fn save_checkpoint(
        &mut self,
        checkpoint: runtime::exec::CheckpointRef,
    ) -> Result<(), runtime::exec::Failure> {
        let expected = self.next_checkpoint()?;
        if checkpoint.build != self.seed.build
            || checkpoint.sequence != expected
            || expected > self.seed.limits.checkpoints
        {
            return Err(runtime::exec::Failure::InvalidCheckpoint);
        }
        self.staged_checkpoints = self.staged_checkpoints.saturating_add(1);
        self.staging.push(ExecStaging::SaveCheckpoint(checkpoint));
        Ok(())
    }

    fn save_checkpoint_bytes(&mut self, bytes: Vec<u8>) -> Result<(), runtime::exec::Failure> {
        let sequence = self.next_checkpoint()?;
        let added =
            u64::try_from(bytes.len()).map_err(|_| runtime::exec::Failure::CheckpointLimit)?;
        if sequence > self.seed.limits.checkpoints
            || self.staged_checkpoint_bytes.saturating_add(added)
                > self.seed.limits.checkpoint_bytes
        {
            return Err(runtime::exec::Failure::CheckpointLimit);
        }
        self.staged_checkpoints = self.staged_checkpoints.saturating_add(1);
        self.staged_checkpoint_bytes = self.staged_checkpoint_bytes.saturating_add(added);
        self.staging.push(ExecStaging::SaveCheckpointBytes(bytes));
        Ok(())
    }

    fn start_child(&mut self, child: runtime::exec::Start) -> Result<(), runtime::exec::Failure> {
        if self.staged_children >= self.seed.limits.child_runs {
            return Err(runtime::exec::Failure::ChildLimit);
        }
        self.staged_children = self.staged_children.saturating_add(1);
        self.staging.push(ExecStaging::StartChild(child));
        Ok(())
    }

    fn stage_output(&mut self, bytes: Vec<u8>) -> Result<(), runtime::exec::Failure> {
        let limit = self.seed.limits.events.max(1);
        if self.staged_outputs >= limit {
            return Err(runtime::exec::Failure::InvalidOutcome);
        }
        self.staged_outputs = self.staged_outputs.saturating_add(1);
        self.staging.push(ExecStaging::StageOutput(bytes));
        Ok(())
    }
}

fn faulted<T>(
    outcome: Result<T, Rejection>,
    fault: &Arc<Mutex<Option<String>>>,
) -> Result<T, Rejection> {
    match fault.lock() {
        Ok(fault) if fault.is_none() => outcome,
        _ => Err(Rejection::ContractViolation),
    }
}

struct RemoteReader {
    host: Arc<dyn Host>,
    lifecycle: bool,
    fault: Arc<Mutex<Option<String>>>,
}

impl RemoteReader {
    fn new(host: Arc<dyn Host>, lifecycle: bool, fault: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            host,
            lifecycle,
            fault,
        }
    }

    fn operation(&self, ordinary: &'static str, lifecycle: &'static str) -> &'static str {
        if self.lifecycle {
            lifecycle
        } else {
            ordinary
        }
    }

    fn call<I: Serialize, O: DeserializeOwned>(&self, operation: &str, input: &I) -> Option<O> {
        match host_call(self.host.as_ref(), operation, input) {
            Ok(value) => Some(value),
            Err(error) => {
                if let Ok(mut fault) = self.fault.lock() {
                    *fault = Some(error);
                }
                None
            }
        }
    }
}

impl BodyReader for RemoteReader {
    fn read_body(&self, key: &BodyKey) -> Result<Option<BodyBytes>, BodyReadFailure> {
        self.call(
            self.operation("context.read_body", "context.lifecycle.read_body"),
            key,
        )
        .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
        .map(|body: Option<Vec<u8>>| body.map(BodyBytes::owned))
    }

    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<Option<CollaborativeBody>, BodyReadFailure> {
        self.call(
            self.operation(
                "context.read_collaborative",
                "context.lifecycle.read_collaborative",
            ),
            key,
        )
        .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
        .map(|body: Option<fabric::CollaborativeView>| body.map(CollaborativeBody::owned))
    }

    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        self.call(
            self.operation(
                "context.bodies_with_schema",
                "context.lifecycle.bodies_with_schema",
            ),
            &SchemaCoordinate {
                world: world.clone(),
                schema: schema.clone(),
            },
        )
        .unwrap_or_default()
    }

    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        let request = PageRequest {
            world: world.clone(),
            schema: schema.clone(),
            after: after.cloned(),
            limit,
        };
        // The two operations answer different shapes: the ordinary snapshot
        // pages infallibly, the frozen lifecycle source reports read failures
        // the way its point reads do.
        if self.lifecycle {
            self.call::<_, Result<Vec<BodyKey>, BodyReadFailure>>(
                "context.lifecycle.body_keys_page",
                &request,
            )
            .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
            .unwrap_or_default()
        } else {
            self.call("context.body_keys_page", &request)
                .unwrap_or_default()
        }
    }

    fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        self.call(
            self.operation("context.body_version", "context.lifecycle.body_version"),
            key,
        )
        .flatten()
    }

    fn anchor_in_body(
        &self,
        key: &BodyKey,
        path: &str,
        position: u64,
    ) -> Result<Option<fabric::Anchor>, BodyReadFailure> {
        self.call(
            "context.anchor",
            &AnchorRequest {
                key: key.clone(),
                path: path.to_string(),
                position,
            },
        )
        .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
    }

    fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, BodyReadFailure> {
        self.call(
            "context.resolve_anchor",
            &ResolveAnchorRequest {
                key: key.clone(),
                anchor: anchor.clone(),
            },
        )
        .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
    }

    fn content_status(&self, content: &replica::content::ContentRef) -> Option<ContentStatus> {
        self.call("context.content_status", content).flatten()
    }

    fn outcome(
        &self,
        world: &WorldId,
        run: runtime::exec::RunId,
        attempt: runtime::exec::AttemptId,
    ) -> Result<Option<OutcomeFacts>, BodyReadFailure> {
        self.call(
            "context.outcome",
            &OutcomeRequest {
                world: world.clone(),
                run,
                attempt,
            },
        )
        .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
    }

    fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.call(
            self.operation("context.body_stamp", "context.lifecycle.body_stamp"),
            key,
        )
        .flatten()
    }
}

struct RemoteFindReader {
    host: Arc<dyn Host>,
    publication: runtime::publication::WorldPublicationId,
    token: Mutex<Option<u64>>,
    fault: Arc<Mutex<Option<String>>>,
}

impl RemoteFindReader {
    fn new(
        host: Arc<dyn Host>,
        publication: runtime::publication::WorldPublicationId,
        fault: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            host,
            publication,
            token: Mutex::new(None),
            fault,
        }
    }

    fn fail(&self, error: String) -> runtime::find::Failure {
        if let Ok(mut fault) = self.fault.lock() {
            *fault = Some(error);
        }
        runtime::find::Failure::Unavailable
    }
}

impl FindReader for RemoteFindReader {
    fn publication(&self) -> runtime::publication::WorldPublicationId {
        self.publication
    }

    fn find(
        &self,
        query: runtime::find::Query,
    ) -> Result<runtime::find::Answer, runtime::find::Failure> {
        let token = self.token.lock().ok().and_then(|token| *token);
        let result: Result<runtime::find::Answer, FindFailure> = match token {
            Some(token) => host_call(
                self.host.as_ref(),
                "find.query_detached",
                &TokenQuery { token, query },
            )
            .map_err(|error| self.fail(error))?,
            None => host_call(self.host.as_ref(), "find.query", &query)
                .map_err(|error| self.fail(error))?,
        };
        result.map_err(Into::into)
    }

    fn acquire_deferred(&self) -> Result<Arc<dyn FindLease>, runtime::find::Failure> {
        let token: u64 = host_call(self.host.as_ref(), "find.acquire_deferred", &())
            .map_err(|error| self.fail(error))?;
        *self
            .token
            .lock()
            .map_err(|_| runtime::find::Failure::Unavailable)? = Some(token);
        Ok(Arc::new(RemoteFindLease {
            host: Arc::clone(&self.host),
            token,
        }))
    }

    fn reserve_analysis(
        &self,
        transient_bytes: u64,
    ) -> Result<AnalyticalMemoryReservation, runtime::find::Failure> {
        let token = self
            .token
            .lock()
            .ok()
            .and_then(|token| *token)
            .ok_or(runtime::find::Failure::Unavailable)?;
        let reservation: Result<u64, FindFailure> = host_call(
            self.host.as_ref(),
            "find.reserve",
            &TokenBytes {
                token,
                bytes: transient_bytes,
            },
        )
        .map_err(|error| self.fail(error))?;
        Ok(AnalyticalMemoryReservation::hosted(Box::new(
            RemoteReservation {
                host: Arc::clone(&self.host),
                token: reservation.map_err(runtime::find::Failure::from)?,
                live: true,
            },
        )))
    }
}

struct RemoteFindLease {
    host: Arc<dyn Host>,
    token: u64,
}

impl FindLease for RemoteFindLease {}

impl Drop for RemoteFindLease {
    fn drop(&mut self) {
        let _ = host_call::<_, ()>(self.host.as_ref(), "find.release", &self.token);
    }
}

struct RemoteReservation {
    host: Arc<dyn Host>,
    token: u64,
    live: bool,
}

impl HostedAnalyticalMemoryReservation for RemoteReservation {
    fn retain(
        mut self: Box<Self>,
        retained_bytes: u64,
    ) -> Result<Box<dyn HostedAnalyticalMemoryLease>, runtime::find::Failure> {
        let lease: Result<u64, FindFailure> = host_call(
            self.host.as_ref(),
            "find.retain",
            &TokenBytes {
                token: self.token,
                bytes: retained_bytes,
            },
        )
        .map_err(|_| runtime::find::Failure::Unavailable)?;
        self.live = false;
        Ok(Box::new(RemoteLease {
            host: Arc::clone(&self.host),
            token: lease.map_err(runtime::find::Failure::from)?,
        }))
    }
}

impl Drop for RemoteReservation {
    fn drop(&mut self) {
        if self.live {
            let _ = host_call::<_, ()>(self.host.as_ref(), "find.release_reservation", &self.token);
        }
    }
}

struct RemoteLease {
    host: Arc<dyn Host>,
    token: u64,
}

impl HostedAnalyticalMemoryLease for RemoteLease {}

impl Drop for RemoteLease {
    fn drop(&mut self) {
        let _ = host_call::<_, ()>(self.host.as_ref(), "find.release_lease", &self.token);
    }
}

/// Child-side facade for the authoritative Session held by the host.
struct RemoteApplicationSession {
    host: Arc<dyn Host>,
    principal: PrincipalFacts,
    world: WorldId,
}

impl RemoteApplicationSession {
    fn new(host: Arc<dyn Host>, principal: PrincipalFacts, world: WorldId) -> Self {
        Self {
            host,
            world,
            principal,
        }
    }

    fn transport_failure(error: String) -> runtime::world::Failure {
        runtime::world::Failure::AuthorityUnavailable(format!(
            "World runner Session callback failed: {error}"
        ))
    }
}

impl SessionAccess for RemoteApplicationSession {
    fn principal_facts(&self) -> Result<PrincipalFacts, Rejection> {
        Ok(self.principal.clone())
    }

    fn space_id(&self) -> &mechanics::ids::SpaceId {
        &self.principal.space
    }

    fn world_id(&self) -> &WorldId {
        &self.world
    }

    fn submit(
        &self,
        action: runtime::world::SignedWorldAction,
    ) -> Result<runtime::world::CommittedEffect, runtime::world::Failure> {
        host_call(self.host.as_ref(), "application.session.submit", &action)
            .map_err(Self::transport_failure)?
    }

    fn submit_lifecycle_from(
        &self,
        action: runtime::world::SignedWorldAction,
        source: LifecycleSourceCoordinate,
    ) -> Result<runtime::world::CommittedEffect, runtime::world::Failure> {
        host_call(
            self.host.as_ref(),
            "application.session.submit_lifecycle",
            &(action, source),
        )
        .map_err(Self::transport_failure)?
    }

    fn query(
        &self,
        query: runtime::world::Query,
    ) -> Result<runtime::world::Projection, runtime::world::Failure> {
        host_call(self.host.as_ref(), "application.session.query", &query)
            .map_err(Self::transport_failure)?
    }

    fn query_at(
        &self,
        publication: runtime::publication::WorldPublicationId,
        query: runtime::world::Query,
    ) -> Result<runtime::world::Projection, runtime::world::Failure> {
        host_call(
            self.host.as_ref(),
            "application.session.query_at",
            &QueryAtRequest { publication, query },
        )
        .map_err(Self::transport_failure)?
    }

    fn find(
        &self,
        query: runtime::find::Query,
    ) -> Result<runtime::find::Answer, runtime::find::Failure> {
        let result: Result<_, FindFailure> =
            host_call(self.host.as_ref(), "application.session.find", &query)
                .map_err(|_| runtime::find::Failure::Unavailable)?;
        result.map_err(Into::into)
    }

    fn find_at(
        &self,
        publication: runtime::publication::WorldPublicationId,
        query: runtime::find::Query,
    ) -> Result<runtime::find::Answer, runtime::find::Failure> {
        let result: Result<_, FindFailure> = host_call(
            self.host.as_ref(),
            "application.session.find_at",
            &FindAtRequest { publication, query },
        )
        .map_err(|_| runtime::find::Failure::Unavailable)?;
        result.map_err(Into::into)
    }

    fn operation_status_for(
        &self,
        operation: runtime::world::RequestId,
        intent: &runtime::world::Intent,
    ) -> Result<runtime::world::OperationStatus, runtime::world::Failure> {
        host_call(
            self.host.as_ref(),
            "application.session.operation_status",
            &(operation, intent),
        )
        .map_err(Self::transport_failure)?
    }

    fn with_lifecycle_source(
        &self,
        source: &LifecycleSourceCoordinate,
        prepare: &mut dyn FnMut(&Context<'_>) -> Result<Vec<u8>, Rejection>,
    ) -> Result<Result<Vec<u8>, Rejection>, runtime::world::Failure> {
        let fault = Arc::new(Mutex::new(None));
        let reader = RemoteLifecycleReader {
            host: Arc::clone(&self.host),
            source: source.clone(),
            fault: Arc::clone(&fault),
        };
        let find = FindHandle::hosted(Arc::new(RemoteLifecycleFindReader {
            host: Arc::clone(&self.host),
            publication: source.publication,
            fault: Arc::clone(&fault),
        }));
        let context = Context::from_runner(
            &self.principal,
            None,
            Some(&reader),
            Some(&self.world),
            None,
            source.publication.publication.manifest_root,
            Some(source.publication),
            Some(find),
            Some(source.clone()),
        );
        let result = prepare(&context);
        let outcome = match fault.lock() {
            Ok(fault) if fault.is_none() => Ok(result),
            Ok(fault) => Err(Self::transport_failure(
                fault
                    .clone()
                    .unwrap_or_else(|| "lifecycle read failed".into()),
            )),
            Err(_) => Err(runtime::world::Failure::CallbackPanicked),
        };
        outcome
    }
}

/// Find over the exact frozen lifecycle source, for a migration planner
/// running in a World process.
///
/// The in-process planner's Context carries Find over that publication; a
/// runner's did not, so every product lookup during planning answered
/// `Unavailable` — masked downstream as corruption — for installed Worlds
/// only. The host already serves exactly this read as
/// `application.session.find_at`.
struct RemoteLifecycleFindReader {
    host: Arc<dyn Host>,
    publication: runtime::publication::WorldPublicationId,
    fault: Arc<Mutex<Option<String>>>,
}

impl FindReader for RemoteLifecycleFindReader {
    fn publication(&self) -> runtime::publication::WorldPublicationId {
        self.publication
    }

    fn find(
        &self,
        query: runtime::find::Query,
    ) -> Result<runtime::find::Answer, runtime::find::Failure> {
        let result: Result<_, FindFailure> = host_call(
            self.host.as_ref(),
            "application.session.find_at",
            &FindAtRequest {
                publication: self.publication,
                query,
            },
        )
        .map_err(|error| {
            if let Ok(mut fault) = self.fault.lock() {
                *fault = Some(error);
            }
            runtime::find::Failure::Unavailable
        })?;
        result.map_err(Into::into)
    }

    fn acquire_deferred(&self) -> Result<Arc<dyn FindLease>, runtime::find::Failure> {
        // Bounded planning steps page synchronously; deferred cursors are not
        // part of the lifecycle read contract.
        Err(runtime::find::Failure::Unavailable)
    }

    fn reserve_analysis(
        &self,
        _transient_bytes: u64,
    ) -> Result<runtime::world::AnalyticalMemoryReservation, runtime::find::Failure> {
        Err(runtime::find::Failure::Unavailable)
    }
}

struct RemoteApplicationIdentity {
    host: Arc<dyn Host>,
    device: mechanics::ids::DeviceId,
}

impl RemoteApplicationIdentity {
    fn new(host: Arc<dyn Host>, device: mechanics::ids::DeviceId) -> Self {
        Self { host, device }
    }
}

impl IdentityAccess for RemoteApplicationIdentity {
    fn device(&self) -> &mechanics::ids::DeviceId {
        &self.device
    }

    fn sign_action(
        &self,
        _session: &dyn SessionAccess,
        request: runtime::world::RequestId,
        intent: runtime::world::Intent,
    ) -> Result<runtime::world::SignedWorldAction, Rejection> {
        host_call(
            self.host.as_ref(),
            "application.identity.sign",
            &SignRequest { request, intent },
        )
        .unwrap_or(Err(Rejection::ContractViolation))
    }
}

struct RemoteClientHost {
    host: Arc<dyn Host>,
    local_root: std::path::PathBuf,
}

impl RemoteClientHost {
    fn call<I: Serialize, O: DeserializeOwned>(
        &self,
        operation: &str,
        input: &I,
    ) -> Result<O, world_interface::Failure> {
        let result: Result<O, world_interface::Failure> =
            host_call(self.host.as_ref(), operation, input).map_err(|error| {
                world_interface::Failure::new(format!("World client transport: {error}"))
            })?;
        result
    }
}

impl world_interface::ClientHost for RemoteClientHost {
    fn local_root(&self) -> &Path {
        &self.local_root
    }

    fn call_world<'a>(
        &'a self,
        call: ApplicationCall,
    ) -> world_interface::ClientFuture<'a, ApplicationReply> {
        Box::pin(async move { self.call("client.host.world", &call) })
    }

    fn call_find<'a>(
        &'a self,
        world: WorldId,
        query: runtime::find::Query,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move { self.call("client.host.find", &(world, query)) })
    }

    fn call_work<'a>(
        &'a self,
        request: runtime::exec::WorkRequest,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move { self.call("client.host.work", &request) })
    }

    fn call_control<'a>(
        &'a self,
        request: world_interface::HostControlRequest,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move { self.call("client.host.control", &request) })
    }

    fn call_content<'a>(
        &'a self,
        request: world_interface::HostContentRequest,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move { self.call("client.host.content", &request) })
    }

    fn call_identity<'a>(
        &'a self,
        handles: Vec<world_interface::PresentationHandle>,
    ) -> world_interface::ClientFuture<'a, world_interface::PresentationResolution> {
        Box::pin(async move { self.call("client.host.identity", &handles) })
    }
}

struct RemoteLifecycleReader {
    host: Arc<dyn Host>,
    source: LifecycleSourceCoordinate,
    fault: Arc<Mutex<Option<String>>>,
}

impl RemoteLifecycleReader {
    fn call<I: Serialize, O: DeserializeOwned>(&self, operation: &str, input: I) -> Option<O> {
        let request = LifecycleReadRequest {
            source: self.source.clone(),
            input,
        };
        match host_call(self.host.as_ref(), operation, &request) {
            Ok(value) => Some(value),
            Err(error) => {
                if let Ok(mut fault) = self.fault.lock() {
                    *fault = Some(error);
                }
                None
            }
        }
    }
}

impl BodyReader for RemoteLifecycleReader {
    fn read_body(&self, key: &BodyKey) -> Result<Option<BodyBytes>, BodyReadFailure> {
        self.call("application.lifecycle.read_body", key.clone())
            .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
            .map(|body: Option<Vec<u8>>| body.map(BodyBytes::owned))
    }

    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<Option<CollaborativeBody>, BodyReadFailure> {
        self.call("application.lifecycle.read_collaborative", key.clone())
            .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
            .map(|body: Option<fabric::CollaborativeView>| body.map(CollaborativeBody::owned))
    }

    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        self.body_keys_page_with_schema(world, schema, None, usize::MAX)
    }

    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        // The host answers the frozen lifecycle source's shape: a Result,
        // like the point reads above, not the ordinary snapshot's bare page.
        self.call::<_, Result<Vec<BodyKey>, BodyReadFailure>>(
            "application.lifecycle.body_keys_page",
            PageRequest {
                world: world.clone(),
                schema: schema.clone(),
                after: after.cloned(),
                limit,
            },
        )
        .unwrap_or(Err(BodyReadFailure::CapabilityUnavailable))
        .unwrap_or_default()
    }

    fn body_version(&self, _key: &BodyKey) -> Option<fabric::Version> {
        None
    }

    fn anchor_in_body(
        &self,
        _key: &BodyKey,
        _path: &str,
        _position: u64,
    ) -> Result<Option<fabric::Anchor>, BodyReadFailure> {
        Err(BodyReadFailure::CapabilityUnavailable)
    }

    fn resolve_anchor(
        &self,
        _key: &BodyKey,
        _anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, BodyReadFailure> {
        Err(BodyReadFailure::CapabilityUnavailable)
    }

    fn content_status(&self, _content: &replica::content::ContentRef) -> Option<ContentStatus> {
        None
    }
}

/// A Runtime [`World`] whose implementation lives in one supervised process.
pub struct RemoteWorld {
    descriptor: Descriptor,
    instance: Mutex<Instance>,
    broker: Arc<Broker>,
    last_failure: Mutex<Option<String>>,
}

/// Process-backed HTTP/MCP client package for one exact [`RemoteWorld`]
/// generation.
#[derive(Clone)]
pub struct RemoteClient {
    world: Arc<RemoteWorld>,
    declaration: ClientDeclaration,
}

struct RemoteExecHandler {
    world: Arc<RemoteWorld>,
    binding: runtime::exec::HandlerBinding,
}

impl runtime::exec::Handler for RemoteExecHandler {
    fn binding(&self) -> &runtime::exec::HandlerBinding {
        &self.binding
    }

    fn handle(
        &self,
        context: &mut dyn runtime::exec::HandlerContext,
    ) -> Result<runtime::exec::Candidate, runtime::exec::Failure> {
        let seed = ExecContextSeed {
            handler: self.binding.clone(),
            resume_checkpoint: context.resume_checkpoint().cloned(),
            committed_checkpoint_count: context.committed_checkpoint_count(),
            world: context.world().clone(),
            run: context.run(),
            attempt: context.attempt(),
            spec: context.spec().clone(),
            build: context.build(),
            input_schema: context.input_schema().clone(),
            input_inline: context.input_inline().to_vec(),
            input_content: context.input_content().to_vec(),
            accepted_resources: context.accepted_resources().to_vec(),
            enforcement_evidence: context.enforcement_evidence(),
            limits: context.limits(),
            links: context.links().to_vec(),
            cancel_asked: context.cancel_asked(),
        };
        let payload = encode(&seed).map_err(|_| runtime::exec::Failure::Handler)?;
        let mut client = self
            .world
            .client()
            .map_err(|_| runtime::exec::Failure::Handler)?;
        let mut callback = |operation: &str, payload: &[u8]| -> Result<Vec<u8>, String> {
            match operation {
                EXEC_FIND => {
                    let query = decode(payload)?;
                    encode(&context.query(query).map_err(ExecFailure::from))
                }
                EXEC_READ_CONTENT => {
                    let (content, offset, len) = decode(payload)?;
                    encode(
                        &context
                            .read_content(&content, offset, len)
                            .map_err(ExecFailure::from),
                    )
                }
                EXEC_CANCEL_ASKED => {
                    let _: () = decode(payload)?;
                    encode(&context.cancel_asked())
                }
                _ => Err(format!("unsupported Exec callback {operation}")),
            }
        };
        let reply = client
            .request_with(
                Operation::Call {
                    operation: EXEC_HANDLE.to_string(),
                    payload,
                },
                &mut callback,
            )
            .map_err(|_| runtime::exec::Failure::Handler)?;
        let Reply::Call { payload } = reply else {
            return Err(runtime::exec::Failure::Handler);
        };
        let completion: Result<ExecCompletion, ExecFailure> =
            decode(&payload).map_err(|_| runtime::exec::Failure::Handler)?;
        let completion = completion.map_err(ExecFailure::runtime)?;
        for staged in completion.staging {
            match staged {
                ExecStaging::SaveCheckpoint(checkpoint) => {
                    context.save_checkpoint(checkpoint)?;
                }
                ExecStaging::SaveCheckpointBytes(bytes) => {
                    context.save_checkpoint_bytes(bytes)?;
                }
                ExecStaging::StartChild(child) => context.start_child(child)?,
                ExecStaging::StageOutput(bytes) => context.stage_output(bytes)?,
            }
        }
        Ok(completion.candidate)
    }
}

/// Build Runtime's executable package from declarations and handlers owned by
/// one exact independently launched World generation.
pub fn remote_exec_package(world: Arc<RemoteWorld>) -> Result<runtime::exec::Package> {
    let declaration: ExecDeclaration = world.invoke_application(EXEC_DESCRIBE, &(), None)?;
    let mut package = world.descriptor.exec_specs.iter().cloned().fold(
        runtime::exec::Package::new(),
        runtime::exec::Package::with_spec,
    );
    for build in declaration.builds {
        package = package.with_build(build);
    }
    for binding in declaration.handlers {
        package = package.with_handler(Arc::new(RemoteExecHandler {
            world: Arc::clone(&world),
            binding,
        }));
    }
    Ok(package)
}

impl RemoteClient {
    pub fn connect(world: Arc<RemoteWorld>) -> Result<Self> {
        let declaration = world.invoke_application(CLIENT_DESCRIBE, &(), None)?;
        Ok(Self { world, declaration })
    }

    pub fn declaration(&self) -> &ClientDeclaration {
        &self.declaration
    }

    fn failure(error: impl std::fmt::Display) -> world_interface::Failure {
        world_interface::Failure::new(format!("World runner client unavailable: {error}"))
    }

    fn parsed(
        &self,
        result: Result<ParsedClientInvocation, world_interface::Failure>,
    ) -> Result<world_interface::ClientInvocation, world_interface::Failure> {
        let parsed = result?;
        Ok(world_interface::ClientInvocation::remote(
            self.world.descriptor.id.clone(),
            parsed.access,
            parsed.confirmation_question,
            parsed.origin,
        ))
    }

    fn request(
        invocation: &world_interface::ClientInvocation,
        host: &dyn world_interface::ClientHost,
    ) -> Result<ClientInvocationRequest, world_interface::Failure> {
        let world_interface::ClientInvocationKind::Remote(origin) = invocation.kind() else {
            return Err(world_interface::Failure::new(
                "process-backed package received a local invocation",
            ));
        };
        Ok(ClientInvocationRequest {
            origin: origin.clone(),
            local_root: host.local_root().to_path_buf(),
        })
    }
}

impl world_interface::ClientAdapter for RemoteClient {
    fn transient_body(&self, document: &str) -> Result<[u8; 16], world_interface::Failure> {
        self.world
            .invoke_application::<_, Result<_, world_interface::Failure>>(
                CLIENT_TRANSIENT_BODY,
                &document.to_string(),
                None,
            )
            .map_err(Self::failure)?
    }

    fn parse_mcp(
        &self,
        tool: &str,
        input: serde_json::Value,
    ) -> Result<world_interface::ClientInvocation, world_interface::Failure> {
        let result = self
            .world
            .invoke_application::<_, Result<ParsedClientInvocation, world_interface::Failure>>(
                CLIENT_PARSE_MCP,
                &(tool.to_string(), input),
                None,
            )
            .map_err(Self::failure)?;
        self.parsed(result)
    }

    fn parse_web(
        &self,
        input: serde_json::Value,
    ) -> Result<world_interface::ClientInvocation, world_interface::Failure> {
        let result = self
            .world
            .invoke_application::<_, Result<ParsedClientInvocation, world_interface::Failure>>(
                CLIENT_PARSE_WEB,
                &input,
                None,
            )
            .map_err(Self::failure)?;
        self.parsed(result)
    }

    fn classify_failure(
        &self,
        value: &serde_json::Value,
    ) -> Option<(world_interface::Failure, String)> {
        self.world
            .invoke_application(CLIENT_CLASSIFY_FAILURE, value, None)
            .ok()
            .flatten()
    }

    fn confirmation<'a>(
        &'a self,
        host: &'a dyn world_interface::ClientHost,
        invocation: &'a world_interface::ClientInvocation,
    ) -> world_interface::ClientFuture<'a, Option<String>> {
        Box::pin(async move {
            let request = Self::request(invocation, host)?;
            invoke_client_async::<_, Result<Option<String>, world_interface::Failure>>(
                Arc::clone(&self.world),
                CLIENT_CONFIRMATION,
                request,
                host,
            )
            .await
            .map_err(Self::failure)?
        })
    }

    fn execute<'a>(
        &'a self,
        host: &'a dyn world_interface::ClientHost,
        invocation: world_interface::ClientInvocation,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let request = Self::request(&invocation, host)?;
            invoke_client_async::<_, Result<serde_json::Value, world_interface::Failure>>(
                Arc::clone(&self.world),
                CLIENT_EXECUTE,
                request,
                host,
            )
            .await
            .map_err(Self::failure)?
        })
    }
}

impl world_interface::display::DisplayAdapter for RemoteClient {
    fn canonicalize_input(
        &self,
        surface: &world_interface::display::DisplaySurfaceId,
        value: serde_json::Value,
    ) -> Result<world_interface::display::CanonicalDisplayInput, world_interface::Failure> {
        self.world
            .invoke_application::<_, Result<_, world_interface::Failure>>(
                CLIENT_DISPLAY_CANONICALIZE,
                &(surface.clone(), value),
                None,
            )
            .map_err(Self::failure)?
    }

    fn prepare(
        &self,
        request: &world_interface::display::DisplayRequest,
    ) -> Result<world_interface::ClientInvocation, world_interface::Failure> {
        let result = self
            .world
            .invoke_application::<_, Result<ParsedClientInvocation, world_interface::Failure>>(
                CLIENT_DISPLAY_PREPARE,
                request,
                None,
            )
            .map_err(Self::failure)?;
        self.parsed(result)
    }

    fn project<'a>(
        &'a self,
        value: serde_json::Value,
        request: &'a world_interface::display::DisplayRequest,
    ) -> world_interface::display::DisplayProjectFuture<'a> {
        let world = Arc::clone(&self.world);
        let request = DisplayProjectRequest {
            value,
            request: request.clone(),
        };
        Box::pin(async move {
            invoke_application_async::<_, Result<_, world_interface::Failure>>(
                world,
                CLIENT_DISPLAY_PROJECT,
                request,
            )
            .await
            .map_err(Self::failure)?
        })
    }

    fn choices_prepare(
        &self,
        surface: &world_interface::display::DisplaySurfaceId,
    ) -> Result<Option<world_interface::ClientInvocation>, world_interface::Failure> {
        let result = self
            .world
            .invoke_application::<_, Result<Option<ParsedClientInvocation>, world_interface::Failure>>(
                CLIENT_DISPLAY_CHOICES_PREPARE,
                surface,
                None,
            )
            .map_err(Self::failure)?;
        result?.map(|parsed| self.parsed(Ok(parsed))).transpose()
    }

    fn choices_project(
        &self,
        surface: &world_interface::display::DisplaySurfaceId,
        value: serde_json::Value,
    ) -> Result<Vec<world_interface::display::DisplayChoice>, world_interface::Failure> {
        self.world
            .invoke_application::<_, Result<_, world_interface::Failure>>(
                CLIENT_DISPLAY_CHOICES_PROJECT,
                &(surface.clone(), value),
                None,
            )
            .map_err(Self::failure)?
    }
}

struct ClientCallback {
    operation: String,
    payload: Vec<u8>,
    reply: std::sync::mpsc::SyncSender<Result<Vec<u8>, String>>,
}

async fn invoke_client_async<I, O>(
    world: Arc<RemoteWorld>,
    operation: &'static str,
    input: I,
    host: &dyn world_interface::ClientHost,
) -> Result<O>
where
    I: Serialize + Send + 'static,
    O: DeserializeOwned + Send + 'static,
{
    let payload = encode(&input).map_err(anyhow::Error::msg)?;
    let (callbacks, mut callback_rx) = tokio::sync::mpsc::unbounded_channel();
    let (complete, mut complete_rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let mut client = world.client()?;
            let mut callback = |operation: &str, payload: &[u8]| {
                let (reply, receive) = std::sync::mpsc::sync_channel(0);
                callbacks
                    .send(ClientCallback {
                        operation: operation.to_string(),
                        payload: payload.to_vec(),
                        reply,
                    })
                    .map_err(|_| "World client callback coordinator stopped".to_string())?;
                receive
                    .recv()
                    .map_err(|_| "World client callback response was lost".to_string())?
            };
            let reply = client.request_with(
                Operation::Call {
                    operation: operation.to_string(),
                    payload,
                },
                &mut callback,
            )?;
            let Reply::Call { payload } = reply else {
                bail!("World client call returned a non-call reply");
            };
            decode(&payload).map_err(anyhow::Error::msg)
        })();
        let _ = complete.send(result);
    });

    loop {
        tokio::select! {
            result = &mut complete_rx => {
                return result.map_err(|_| anyhow!("World client worker stopped"))?;
            }
            callback = callback_rx.recv() => {
                let Some(callback) = callback else {
                    return complete_rx.await.map_err(|_| anyhow!("World client worker stopped"))?;
                };
                let answer = client_host_callback(host, &callback.operation, &callback.payload).await;
                let _ = callback.reply.send(answer);
            }
        }
    }
}

async fn invoke_application_async<I, O>(
    world: Arc<RemoteWorld>,
    operation: &'static str,
    input: I,
) -> Result<O>
where
    I: Serialize + Send + 'static,
    O: DeserializeOwned + Send + 'static,
{
    let (complete, complete_rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = world.invoke_application(operation, &input, None);
        let _ = complete.send(result);
    });
    complete_rx
        .await
        .map_err(|_| anyhow!("World application worker stopped"))?
}

impl std::fmt::Debug for RemoteWorld {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteWorld")
            .field("world", &self.descriptor.id)
            .finish_non_exhaustive()
    }
}

impl RemoteWorld {
    pub fn connect(mut instance: Instance) -> Result<Self> {
        let service = instance.service().clone();
        let mut client = instance.client()?;
        let descriptor: Descriptor = call(&mut client, DESCRIBE, &(), None, None)?;
        if descriptor.id.to_string() != service.world
            || descriptor.implementation_version.0 != service.implementation_version
        {
            bail!("World runner descriptor does not match its readiness declaration");
        }
        Ok(Self {
            descriptor,
            instance: Mutex::new(instance),
            broker: Arc::new(Broker::default()),
            last_failure: Mutex::new(None),
        })
    }

    pub fn reviewed_implementation(&self) -> [u8; 32] {
        self.instance
            .lock()
            .map(|instance| instance.service().implementation)
            .unwrap_or([0; 32])
    }

    /// The most recent transport/protocol failure, for host diagnostics.
    pub fn last_failure(&self) -> Option<String> {
        self.last_failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
    }

    fn unavailable<T>(&self, error: anyhow::Error) -> Result<T, Rejection> {
        if let Ok(mut failure) = self.last_failure.lock() {
            *failure = Some(format!("{error:#}"));
        }
        Err(Rejection::ImplementationUnavailable)
    }

    fn context_seed(context: &Context<'_>) -> ContextSeed {
        let capabilities = context.runner_capabilities();
        ContextSeed {
            principal: context.principal().clone(),
            manifest_root: context.manifest_root(),
            publication: context.world_publication_id(),
            request: context.request_id(),
            lifecycle_source: context.lifecycle_source().cloned(),
            has_reads: capabilities.reads,
            has_find: capabilities.find,
        }
    }

    /// Prepare transport while holding the supervision lock only long enough
    /// to detect/restart a dead process and allocate a request identifier.
    /// The request itself must run after the lock is released because its host
    /// callbacks may synchronously re-enter this same World generation.
    fn client(&self) -> Result<RequestClient> {
        let mut instance = self
            .instance
            .lock()
            .map_err(|_| anyhow!("World runner process lock was poisoned"))?;
        instance.client()
    }

    fn invoke<I: Serialize, O: DeserializeOwned>(
        &self,
        operation: &str,
        input: &I,
        context: Option<&Context<'_>>,
        extraction: Option<&ExtractionContext<'_>>,
    ) -> Result<O> {
        let mut client = self.client()?;
        call(
            &mut client,
            operation,
            input,
            context.map(|context| (context, Arc::clone(&self.broker))),
            extraction,
        )
    }

    fn invoke_application<I: Serialize, O: DeserializeOwned>(
        &self,
        operation: &str,
        input: &I,
        context: Option<&ApplicationContext<'_>>,
    ) -> Result<O> {
        let mut client = self.client()?;
        let payload = encode(input).map_err(anyhow::Error::msg)?;
        let mut callback = |operation: &str, payload: &[u8]| match context {
            Some(context) => {
                application_callback(context.session, Some(context.identity), operation, payload)
            }
            None => Err(format!(
                "World called host operation {operation:?} without an application context"
            )),
        };
        let reply = client.request_with(
            Operation::Call {
                operation: operation.to_string(),
                payload,
            },
            &mut callback,
        )?;
        let Reply::Call { payload } = reply else {
            bail!("World application call returned a non-call reply");
        };
        decode(&payload).map_err(anyhow::Error::msg)
    }

    fn invoke_application_with_access<I: Serialize, O: DeserializeOwned>(
        &self,
        operation: &str,
        input: &I,
        session: &dyn SessionAccess,
        identity: Option<&dyn IdentityAccess>,
    ) -> Result<O> {
        let mut client = self.client()?;
        let payload = encode(input).map_err(anyhow::Error::msg)?;
        let mut callback = |operation: &str, payload: &[u8]| {
            application_callback(session, identity, operation, payload)
        };
        let reply = client.request_with(
            Operation::Call {
                operation: operation.to_string(),
                payload,
            },
            &mut callback,
        )?;
        let Reply::Call { payload } = reply else {
            bail!("World application call returned a non-call reply");
        };
        decode(&payload).map_err(anyhow::Error::msg)
    }

    fn session_seed(session: &dyn SessionAccess) -> Result<ApplicationContextSeed> {
        Ok(ApplicationContextSeed {
            principal: session
                .principal_facts()
                .map_err(|error| anyhow!("resolve application principal: {error}"))?,
        })
    }

    fn application_seed(context: &ApplicationContext<'_>) -> Result<ApplicationContextSeed> {
        let principal = Self::session_seed(context.session)?.principal;
        if principal.actor.as_str() != context.actor || principal.device.as_str() != context.device
        {
            bail!("application context identity does not match its authoritative Session")
        }
        Ok(ApplicationContextSeed { principal })
    }
}

impl World for RemoteWorld {
    fn descriptor(&self) -> Descriptor {
        self.descriptor.clone()
    }

    fn id(&self) -> WorldId {
        self.descriptor.id.clone()
    }

    fn schemas(&self) -> &[replica::body::Schema] {
        &self.descriptor.schemas
    }

    fn scope_schemas(&self) -> &[runtime::world::ScopeSchema] {
        &self.descriptor.scope_schemas
    }

    fn signal_schemas(&self) -> &[runtime::world::SignalSchema] {
        &self.descriptor.signal_schemas
    }

    fn find_schemas(&self) -> &[runtime::find::Schema] {
        &self.descriptor.find_schemas
    }

    fn find_extractors(&self) -> &[runtime::find::Extractor] {
        &self.descriptor.find_extractors
    }

    fn exec_specs(&self) -> &[runtime::exec::Spec] {
        &self.descriptor.exec_specs
    }

    fn extract(
        &self,
        context: &ExtractionContext<'_>,
        extractor: &runtime::find::Extractor,
        body: &BodyKey,
    ) -> Result<runtime::find::BodyExtraction, Rejection> {
        self.invoke::<_, Result<_, Rejection>>(
            EXTRACT,
            &ExtractRequest {
                publication: context.world_publication_id(),
                extractor: extractor.clone(),
                body: body.clone(),
            },
            None,
            Some(context),
        )
        .unwrap_or_else(|error| self.unavailable(error))
    }

    fn submit(&self, context: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        self.invoke::<_, Result<_, Rejection>>(
            SUBMIT,
            &SubmitRequest {
                context: Self::context_seed(context),
                intent,
            },
            Some(context),
            None,
        )
        .unwrap_or_else(|error| self.unavailable(error))
    }

    fn query(&self, context: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        self.invoke::<_, Result<_, Rejection>>(
            QUERY,
            &QueryRequest {
                context: Self::context_seed(context),
                query,
            },
            Some(context),
            None,
        )
        .unwrap_or_else(|error| self.unavailable(error))
    }
}

impl Handler for RemoteWorld {
    fn access(&self, call: &ApplicationCall) -> Result<CallAccess, CallFailure> {
        self.invoke_application(APPLICATION_ACCESS, call, None)
            .unwrap_or_else(|_| {
                Err(CallFailure::new(
                    runtime::world::call::Code::Unavailable,
                    "World runner is unavailable",
                ))
            })
    }

    fn call(&self, call: &ApplicationCall, context: &ApplicationContext<'_>) -> ApplicationReply {
        let request = Self::application_seed(context).map(|context| ApplicationCallRequest {
            context,
            call: call.clone(),
        });
        request
            .and_then(|request| self.invoke_application(APPLICATION_CALL, &request, Some(context)))
            .unwrap_or_else(|_| {
                ApplicationReply::error(
                    call,
                    runtime::world::call::Code::Unavailable,
                    "World runner is unavailable",
                )
            })
    }

    fn nudges(
        &self,
        call: &ApplicationCall,
        reply: &ApplicationReply,
        context: &ApplicationContext<'_>,
    ) -> Vec<Nudge> {
        let request = Self::application_seed(context).map(|context| ApplicationNudgesRequest {
            context,
            call: call.clone(),
            reply: reply.clone(),
        });
        request
            .and_then(|request| {
                self.invoke_application(APPLICATION_NUDGES, &request, Some(context))
            })
            .unwrap_or_default()
    }
}

impl WorldApplication for RemoteWorld {
    fn founder_grants(&self) -> anyhow::Result<Vec<FounderGrant>> {
        self.invoke_application::<_, Result<_, String>>(APPLICATION_FOUNDER_GRANTS, &(), None)?
            .map_err(anyhow::Error::msg)
    }

    fn admission_evidence(
        &self,
        role: &str,
        parent_manifest_root: [u8; 32],
    ) -> anyhow::Result<Option<mechanics::authorization::WorldAssignmentEvidence>> {
        self.invoke_application::<_, Result<_, String>>(
            APPLICATION_ADMISSION_EVIDENCE,
            &(role.to_string(), parent_manifest_root),
            None,
        )?
        .map_err(anyhow::Error::msg)
    }

    fn initial_scope(&self, display_name: &str) -> Option<InitialScope> {
        self.invoke_application(APPLICATION_INITIAL_SCOPE, &display_name.to_string(), None)
            .ok()
            .flatten()
    }

    fn bootstrap(&self, context: BootstrapContext<'_>) -> anyhow::Result<()> {
        let seed = Self::session_seed(context.session)?;
        if &seed.principal.device != context.identity.device() {
            bail!("formation identity does not match its authoritative Session");
        }
        let request = BootstrapRequest {
            context: seed,
            store_root: context.store_root.to_path_buf(),
            space: context.space.clone(),
            device: context.device.to_string(),
            display_name: context.display_name.to_string(),
            initial_scope: context.initial_scope.cloned(),
        };
        self.invoke_application_with_access::<_, Result<(), String>>(
            APPLICATION_BOOTSTRAP,
            &request,
            context.session,
            Some(context.identity),
        )?
        .map_err(anyhow::Error::msg)
    }

    fn assess_upgrade(
        &self,
        active: Option<ReviewedImplementation>,
        preferred: ReviewedImplementation,
    ) -> anyhow::Result<WorldUpgradeAssessment> {
        self.invoke_application::<_, Result<_, String>>(
            APPLICATION_ASSESS_UPGRADE,
            &(active, preferred),
            None,
        )?
        .map_err(anyhow::Error::msg)
    }

    fn verification_migrator(
        &self,
        preferred: ReviewedImplementation,
    ) -> Option<ReviewedImplementation> {
        self.invoke_application(APPLICATION_VERIFICATION_MIGRATOR, &preferred, None)
            .ok()
            .flatten()
    }

    fn upgrade_step(
        &self,
        context: WorldUpgradeContext<'_>,
    ) -> anyhow::Result<WorldUpgradeProgress> {
        let seed = Self::session_seed(context.session)?;
        if &seed.principal.device != context.identity.device() {
            bail!("migration identity does not match its authoritative Session");
        }
        let request = UpgradeStepRequest {
            context: seed,
            space: context.space.clone(),
            device: context.device.to_string(),
            active: context.active,
            migrator: context.migrator,
            preferred: context.preferred,
            source: context.source.clone(),
            record: context.record.map(<[u8]>::to_vec),
        };
        self.invoke_application_with_access::<_, Result<_, String>>(
            APPLICATION_UPGRADE_STEP,
            &request,
            context.session,
            Some(context.identity),
        )?
        .map_err(anyhow::Error::msg)
    }

    fn status(&self, session: &dyn SessionAccess) -> Option<StatusProjection> {
        let seed = Self::session_seed(session).ok()?;
        self.invoke_application_with_access(APPLICATION_STATUS, &seed, session, None)
            .ok()
            .flatten()
    }

    fn start_projector(&self, session: &dyn SessionAccess, space: &mechanics::ids::SpaceId) {
        let Ok(seed) = Self::session_seed(session) else {
            return;
        };
        let _: Result<()> = self.invoke_application_with_access(
            APPLICATION_START_PROJECTOR,
            &ProjectorRequest {
                context: seed,
                space: space.clone(),
            },
            session,
            None,
        );
    }

    fn project(
        &self,
        session: &dyn SessionAccess,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> runtime::world::Invalidation {
        let Ok(seed) = Self::session_seed(session) else {
            return runtime::world::Invalidation::default();
        };
        self.invoke_application_with_access(
            APPLICATION_PROJECT,
            &ProjectRequest {
                context: seed,
                space: space.clone(),
                observation: observation.clone(),
            },
            session,
            None,
        )
        .unwrap_or_default()
    }
}

fn call<I: Serialize, O: DeserializeOwned>(
    client: &mut RequestClient,
    operation: &str,
    input: &I,
    context: Option<(&Context<'_>, Arc<Broker>)>,
    extraction: Option<&ExtractionContext<'_>>,
) -> Result<O> {
    let payload = encode(input).map_err(anyhow::Error::msg)?;
    let detached: Arc<dyn CallbackHandler> = context.as_ref().map_or_else(
        || Arc::new(Broker::default()) as Arc<dyn CallbackHandler>,
        |(_, broker)| broker.clone(),
    );
    let mut callback = |operation: &str, payload: &[u8]| match (&context, extraction) {
        (Some((context, broker)), _) => context_callback(context, broker, operation, payload),
        (None, Some(context)) => extraction_callback(context, operation, payload),
        (None, None) => Err(format!(
            "World called host operation {operation:?} without a context"
        )),
    };
    let reply = client.request_with_detached(
        Operation::Call {
            operation: operation.to_string(),
            payload,
        },
        &mut callback,
        detached,
    )?;
    let Reply::Call { payload } = reply else {
        bail!("World semantic call returned a non-call reply");
    };
    decode(&payload).map_err(|error| anyhow!("{operation}: {error}"))
}

fn context_callback(
    context: &Context<'_>,
    broker: &Arc<Broker>,
    operation: &str,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    match operation {
        "context.read_body" => {
            let key: BodyKey = decode(payload)?;
            encode(
                &context
                    .read_body(&key)
                    .map(|body| body.map(|body| body.to_vec())),
            )
        }
        "context.lifecycle.read_body" => {
            let key: BodyKey = decode(payload)?;
            encode(
                &context
                    .read_lifecycle_source_body(&key)
                    .map(|body| body.map(|body| body.to_vec())),
            )
        }
        "context.read_collaborative" => {
            let key: BodyKey = decode(payload)?;
            encode(
                &context
                    .read_collaborative(&key)
                    .map(|body| body.map(|body| body.as_ref().clone())),
            )
        }
        "context.lifecycle.read_collaborative" => {
            let key: BodyKey = decode(payload)?;
            encode(
                &context
                    .read_lifecycle_source_collaborative(&key)
                    .map(|body| body.map(|body| body.as_ref().clone())),
            )
        }
        "context.bodies_with_schema" => {
            let request: SchemaCoordinate = decode(payload)?;
            encode(&context.bodies_with_schema(&request.world, &request.schema))
        }
        "context.body_keys_page" => {
            let request: PageRequest = decode(payload)?;
            encode(&context.body_keys_page_with_schema(
                &request.world,
                &request.schema,
                request.after.as_ref(),
                request.limit,
            ))
        }
        "context.lifecycle.body_keys_page" => {
            let request: PageRequest = decode(payload)?;
            encode(&context.lifecycle_source_body_keys_page_with_schema(
                &request.world,
                &request.schema,
                request.after.as_ref(),
                request.limit,
            ))
        }
        "context.body_version" => {
            let key: BodyKey = decode(payload)?;
            encode(&context.body_version(&key))
        }
        "context.body_stamp" => {
            let key: BodyKey = decode(payload)?;
            encode(&context.body_stamp(&key))
        }
        "context.anchor" => {
            let request: AnchorRequest = decode(payload)?;
            encode(&context.anchor(&request.key, &request.path, request.position))
        }
        "context.resolve_anchor" => {
            let request: ResolveAnchorRequest = decode(payload)?;
            encode(&context.resolve_anchor(&request.key, &request.anchor))
        }
        "context.content_status" => {
            let content = decode(payload)?;
            encode(&context.content_status(&content))
        }
        "context.outcome" => {
            let request: OutcomeRequest = decode(payload)?;
            encode(&context.outcome(request.run, request.attempt))
        }
        "find.query" => {
            let query = decode(payload)?;
            let result = context.find(query).map_err(FindFailure::from);
            encode(&result)
        }
        "find.acquire_deferred" => {
            let _: () = decode(payload)?;
            let handle = context
                .deferred_find()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "this invocation has no deferred Find capability".to_string())?;
            encode(&broker.insert_find(handle))
        }
        _ if operation.starts_with("find.") => broker.call(operation, payload),
        _ => Err(format!("unsupported World context callback {operation}")),
    }
}

fn lifecycle_callback(
    session: &dyn SessionAccess,
    source: &LifecycleSourceCoordinate,
    mut read: impl FnMut(&Context<'_>) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let mut prepare =
        |lifecycle: &Context<'_>| read(lifecycle).map_err(|_| Rejection::ContractViolation);
    session
        .with_lifecycle_source(source, &mut prepare)
        .map_err(|error| format!("open application lifecycle source: {error:?}"))?
        .map_err(|error| format!("read application lifecycle source: {error:?}"))
}

fn application_callback(
    session: &dyn SessionAccess,
    identity: Option<&dyn IdentityAccess>,
    operation: &str,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    match operation {
        "application.session.submit" => {
            let action = decode(payload)?;
            encode(&session.submit(action))
        }
        "application.session.submit_lifecycle" => {
            let (action, source) = decode(payload)?;
            encode(&session.submit_lifecycle_from(action, source))
        }
        "application.session.query" => {
            let query = decode(payload)?;
            encode(&session.query(query))
        }
        "application.session.query_at" => {
            let request: QueryAtRequest = decode(payload)?;
            encode(&session.query_at(request.publication, request.query))
        }
        "application.session.find" => {
            let query = decode(payload)?;
            encode(&session.find(query).map_err(FindFailure::from))
        }
        "application.session.find_at" => {
            let request: FindAtRequest = decode(payload)?;
            encode(
                &session
                    .find_at(request.publication, request.query)
                    .map_err(FindFailure::from),
            )
        }
        "application.session.operation_status" => {
            let (request, intent) = decode(payload)?;
            encode(&session.operation_status_for(request, &intent))
        }
        "application.identity.sign" => {
            let request: SignRequest = decode(payload)?;
            let identity = identity.ok_or_else(|| {
                "World requested signing outside a lifecycle or call invocation".to_string()
            })?;
            encode(&identity.sign_action(session, request.request, request.intent))
        }
        "application.lifecycle.read_body" => {
            let request: LifecycleReadRequest<BodyKey> = decode(payload)?;
            lifecycle_callback(session, &request.source, |lifecycle| {
                encode(
                    &lifecycle
                        .read_lifecycle_source_body(&request.input)
                        .map(|body| body.map(|body| body.to_vec())),
                )
            })
        }
        "application.lifecycle.read_collaborative" => {
            let request: LifecycleReadRequest<BodyKey> = decode(payload)?;
            lifecycle_callback(session, &request.source, |lifecycle| {
                encode(
                    &lifecycle
                        .read_lifecycle_source_collaborative(&request.input)
                        .map(|body| body.map(|body| body.as_ref().clone())),
                )
            })
        }
        "application.lifecycle.body_keys_page" => {
            let request: LifecycleReadRequest<PageRequest> = decode(payload)?;
            lifecycle_callback(session, &request.source, |lifecycle| {
                encode(&lifecycle.lifecycle_source_body_keys_page_with_schema(
                    &request.input.world,
                    &request.input.schema,
                    request.input.after.as_ref(),
                    request.input.limit,
                ))
            })
        }
        _ => Err(format!(
            "unsupported World application callback {operation}"
        )),
    }
}

async fn client_host_callback(
    host: &dyn world_interface::ClientHost,
    operation: &str,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    match operation {
        "client.host.world" => {
            let call = decode(payload)?;
            encode(&host.call_world(call).await)
        }
        "client.host.find" => {
            let (world, query) = decode(payload)?;
            encode(&host.call_find(world, query).await)
        }
        "client.host.work" => {
            let request = decode(payload)?;
            encode(&host.call_work(request).await)
        }
        "client.host.control" => {
            let request = decode(payload)?;
            encode(&host.call_control(request).await)
        }
        "client.host.content" => {
            let request = decode(payload)?;
            encode(&host.call_content(request).await)
        }
        "client.host.identity" => {
            let handles = decode(payload)?;
            encode(&host.call_identity(handles).await)
        }
        _ => Err(format!("unsupported World client callback {operation}")),
    }
}

fn extraction_callback(
    context: &ExtractionContext<'_>,
    operation: &str,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    match operation {
        "context.read_body" => {
            let key: BodyKey = decode(payload)?;
            encode(
                &context
                    .read_body(&key)
                    .map(|body| body.map(|body| body.to_vec())),
            )
        }
        "context.read_collaborative" => {
            let key: BodyKey = decode(payload)?;
            encode(
                &context
                    .read_collaborative(&key)
                    .map(|body| body.map(|body| body.as_ref().clone())),
            )
        }
        "context.body_stamp" => {
            let key: BodyKey = decode(payload)?;
            encode(&context.body_stamp(&key))
        }
        _ => Err(format!("unsupported World extraction callback {operation}")),
    }
}

#[derive(Default)]
struct Broker {
    next: AtomicU64,
    find: Mutex<BTreeMap<u64, FindHandle>>,
    reservations: Mutex<BTreeMap<u64, AnalyticalMemoryReservation>>,
    leases: Mutex<BTreeMap<u64, AnalyticalMemoryLease>>,
}

impl Broker {
    fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    fn insert_find(&self, handle: FindHandle) -> u64 {
        let token = self.next();
        if let Ok(mut handles) = self.find.lock() {
            handles.insert(token, handle);
        }
        token
    }
}

impl CallbackHandler for Broker {
    fn call(&self, operation: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
        match operation {
            "find.query_detached" => {
                let request: TokenQuery = decode(payload)?;
                let handles = self.find.lock().map_err(|_| "Find broker was poisoned")?;
                let handle = handles
                    .get(&request.token)
                    .ok_or_else(|| "detached Find capability expired".to_string())?;
                encode(&handle.find(request.query).map_err(FindFailure::from))
            }
            "find.reserve" => {
                let request: TokenBytes = decode(payload)?;
                let reservation = {
                    let handles = self.find.lock().map_err(|_| "Find broker was poisoned")?;
                    let handle = handles
                        .get(&request.token)
                        .ok_or_else(|| "detached Find capability expired".to_string())?;
                    handle.reserve_analysis(request.bytes)
                };
                match reservation {
                    Ok(reservation) => {
                        let token = self.next();
                        self.reservations
                            .lock()
                            .map_err(|_| "Find reservation broker was poisoned")?
                            .insert(token, reservation);
                        encode(&Ok::<_, FindFailure>(token))
                    }
                    Err(error) => encode(&Err::<u64, _>(FindFailure::from(error))),
                }
            }
            "find.retain" => {
                let request: TokenBytes = decode(payload)?;
                let reservation = self
                    .reservations
                    .lock()
                    .map_err(|_| "Find reservation broker was poisoned")?
                    .remove(&request.token)
                    .ok_or_else(|| "analytical reservation expired".to_string())?;
                match reservation.retain(request.bytes) {
                    Ok(lease) => {
                        let token = self.next();
                        self.leases
                            .lock()
                            .map_err(|_| "Find lease broker was poisoned")?
                            .insert(token, lease);
                        encode(&Ok::<_, FindFailure>(token))
                    }
                    Err(error) => encode(&Err::<u64, _>(FindFailure::from(error))),
                }
            }
            "find.release" => {
                let token: u64 = decode(payload)?;
                self.find
                    .lock()
                    .map_err(|_| "Find broker was poisoned")?
                    .remove(&token);
                encode(&())
            }
            "find.release_reservation" => {
                let token: u64 = decode(payload)?;
                self.reservations
                    .lock()
                    .map_err(|_| "Find reservation broker was poisoned")?
                    .remove(&token);
                encode(&())
            }
            "find.release_lease" => {
                let token: u64 = decode(payload)?;
                self.leases
                    .lock()
                    .map_err(|_| "Find lease broker was poisoned")?
                    .remove(&token);
                encode(&())
            }
            _ => Err(format!("unsupported detached World callback {operation}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use world_interface::ClientHost as _;

    #[test]
    fn runtime_codec_round_trips_json_values() {
        let value = serde_json::json!({
            "nested": [true, null, {"count": 7}],
            "text": "a World declaration"
        });
        let encoded = super::encode(&value).expect("JSON encodes inside the bounded ABI payload");
        let decoded: serde_json::Value =
            super::decode(&encoded).expect("JSON decodes without deserialize_any refusal");
        assert_eq!(decoded, value);
    }

    #[test]
    fn runtime_codec_round_trips_flattened_world_replies() {
        let world = replica::body::WorldId::parse("com.lait.codec").expect("valid World id");
        let call = runtime::world::call::Call::new(world, "catalog.read", 1, vec![1, 2, 3])
            .expect("valid World call");
        let reply = runtime::world::call::Reply::ok(&call, vec![4, 5, 6]);
        let result = Ok::<_, world_interface::Failure>(reply.clone());

        let encoded = super::encode(&result).expect("flattened reply encodes");
        let decoded: Result<runtime::world::call::Reply, world_interface::Failure> =
            super::decode(&encoded).expect("flattened reply decodes");
        assert_eq!(decoded.expect("successful World reply"), reply);
    }

    #[test]
    fn remote_client_host_decodes_one_application_result_layer() {
        struct ReplyHost(Vec<u8>);

        impl world_runner::Host for ReplyHost {
            fn call(&self, operation: &str, _payload: &[u8]) -> Result<Vec<u8>, String> {
                assert_eq!(operation, "client.host.world");
                Ok(self.0.clone())
            }
        }

        let world = replica::body::WorldId::parse("com.lait.codec").expect("valid World id");
        let call = runtime::world::call::Call::new(world, "catalog.read", 1, vec![1, 2, 3])
            .expect("valid World call");
        let reply = runtime::world::call::Reply::ok(&call, vec![4, 5, 6]);
        let payload = super::encode(&Ok::<_, world_interface::Failure>(reply.clone()))
            .expect("callback reply encodes");
        let host = super::RemoteClientHost {
            host: Arc::new(ReplyHost(payload)),
            local_root: PathBuf::new(),
        };

        let decoded = futures_lite::future::block_on(host.call_world(call))
            .expect("callback reply contains one result layer");
        assert_eq!(decoded, reply);
    }
}
