#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! Client-facing application interfaces supplied by a World package.
//!
//! Runtime's World-call boundary deliberately knows nothing about how an answer
//! is displayed, and neither does this crate. It is the outer runner-neutral
//! seam: a World declares its mount name, its MCP tools, and how to decode a
//! reply into a value; the application shell supplies process
//! lifecycle, Orbit selection, transport, and every byte a human eventually
//! reads.
//!
//! Nothing here renders. A head that wants a table, a terminal line, or an HTML
//! page builds it from the [`serde_json::Value`] an invocation answers with —
//! which is why executing one costs a decode and nothing else.

pub mod destination;
pub mod display;
pub mod manifest;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use replica::body::WorldId;
use runtime::world::call::{Call, Reply};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable classification of a client-surface failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    /// A declaration, invocation, or returned value was invalid.
    Invalid,
    /// A valid operation was refused by the selected surface.
    Refusal,
    /// An accepted operation could not be completed.
    Operation,
    /// An established client operation ended before completion.
    Interruption,
}

/// A typed client-surface failure.
///
/// Adapter diagnostics remain separate from the stable classification. Most
/// boundaries deliberately render only the classification; an argument-owning
/// surface such as MCP may explicitly preserve [`Self::diagnostic`] so callers
/// can repair malformed input without depending on a tracing subscriber.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    kind: FailureKind,
    diagnostic: Option<String>,
}

impl Failure {
    pub fn new(message: impl fmt::Display) -> Self {
        let diagnostic = message.to_string();
        tracing::warn!(%diagnostic, "World client adapter rejected an operation");
        Self {
            kind: FailureKind::Invalid,
            diagnostic: Some(diagnostic),
        }
    }

    pub const fn invalid() -> Self {
        Self {
            kind: FailureKind::Invalid,
            diagnostic: None,
        }
    }

    pub const fn refusal() -> Self {
        Self {
            kind: FailureKind::Refusal,
            diagnostic: None,
        }
    }

    pub const fn operation() -> Self {
        Self {
            kind: FailureKind::Operation,
            diagnostic: None,
        }
    }

    pub const fn interruption() -> Self {
        Self {
            kind: FailureKind::Interruption,
            diagnostic: None,
        }
    }

    pub const fn kind(&self) -> FailureKind {
        self.kind
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self.kind {
            FailureKind::Invalid => "invalid client operation",
            FailureKind::Refusal => "client operation refused",
            FailureKind::Operation => "client operation failed",
            FailureKind::Interruption => "client operation interrupted",
        };
        match self.diagnostic() {
            // The diagnostic is the World's own words; a label that hides
            // them turns every distinct refusal into the same sentence.
            Some(diagnostic) => write!(f, "{label}: {diagnostic}"),
            None => f.write_str(label),
        }
    }
}

impl std::error::Error for Failure {}

/// The externally visible effect of one parsed invocation, classified by the
/// package that parsed it.
///
/// Every invocation carries one, including a World call: the package derives it
/// from the very request it then encodes, so it costs nothing and cannot
/// disagree with the bytes. It exists so a *head* can apply its own policy —
/// "this route serves reads only" — without decoding the call a second time on
/// the request path.
///
/// It is not the authorization answer, and no grant is ever checked against it.
/// The daemon re-classifies every World call through
/// [`runtime::world::call::Access`] on its own side of the boundary, and that
/// classification is the one authorization consults. Treating this one as
/// authoritative would be trusting a value computed before the call left the
/// process that made it.
///
/// For a local operation there is no such second answer: local operations —
/// advancing a read watermark, writing an attachment to disk, committing a
/// grant through Space authority — never reach a World Handler, so the package
/// that implements one is the only code that can classify it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientAccess {
    Query,
    Command,
}

/// One package-owned local operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalInvocation {
    pub operation: String,
    pub input: Value,
}

/// Shape one admitted Runtime Find answer for a product-owned client surface.
///
/// Runtime owns evaluation and the exact answer type. The product owns the
/// names, cursor spelling, and row envelope it publishes to an agent or human.
/// Keeping this as a runner-local function pointer avoids teaching the core host
/// any product vocabulary while still refusing adapters that reinterpret raw JSON.
pub type FindResponsePresenter = fn(runtime::find::Answer) -> Result<Value, Failure>;

/// The target selected by a parsed product invocation.
#[derive(Debug, Clone)]
pub enum ClientInvocationKind {
    World(Call),
    /// One product-owned convenience query compiled to Runtime's common Find
    /// algebra. The host still supplies actor, device, authority and Station
    /// coordinates; a package can select fields but cannot bypass admission.
    Find {
        query: runtime::find::Query,
        presenter: Option<FindResponsePresenter>,
    },
    Local(LocalInvocation),
    /// Opaque parse material owned by a process-backed package. The host may
    /// inspect only the product-neutral access and confirmation metadata on
    /// [`ClientInvocation`]; the package re-parses these bounded bytes before
    /// confirmation and execution.
    Remote(Vec<u8>),
}

/// A parsed product invocation with package-owned policy metadata.
///
/// `Find` operations let a package compile friendly product filters to the
/// common Runtime query algebra. `Local` operations may compose World calls
/// with working-tree, filesystem, caller-local state, or generic
/// Space-authority facilities. The shell enforces the declared whole-operation
/// access and confirmation policy, then routes execution without interpreting
/// product vocabulary.
#[derive(Debug, Clone)]
pub struct ClientInvocation {
    world: WorldId,
    access: ClientAccess,
    confirmation_question: Option<String>,
    kind: ClientInvocationKind,
}

impl ClientInvocation {
    pub fn world(call: Call, access: ClientAccess, confirmation_question: Option<String>) -> Self {
        Self {
            world: call.world().clone(),
            access,
            confirmation_question,
            kind: ClientInvocationKind::World(call),
        }
    }

    pub fn local(
        world: WorldId,
        operation: impl Into<String>,
        input: Value,
        access: ClientAccess,
        confirmation_question: Option<String>,
    ) -> Self {
        Self {
            world,
            access,
            confirmation_question,
            kind: ClientInvocationKind::Local(LocalInvocation {
                operation: operation.into(),
                input,
            }),
        }
    }

    /// Construct a read-only package invocation over Runtime Find.
    pub fn find(world: WorldId, query: runtime::find::Query) -> Self {
        Self {
            world,
            access: ClientAccess::Query,
            confirmation_question: None,
            kind: ClientInvocationKind::Find {
                query,
                presenter: None,
            },
        }
    }

    /// Construct a read-only Find invocation with product-owned presentation.
    ///
    /// The host still evaluates the same admitted query and raw root Find
    /// remains available through [`Self::find`]. Only the successful answer's
    /// outer client representation changes.
    pub fn find_presented(
        world: WorldId,
        query: runtime::find::Query,
        presenter: FindResponsePresenter,
    ) -> Self {
        Self {
            world,
            access: ClientAccess::Query,
            confirmation_question: None,
            kind: ClientInvocationKind::Find {
                query,
                presenter: Some(presenter),
            },
        }
    }

    pub fn remote(
        world: WorldId,
        access: ClientAccess,
        confirmation_question: Option<String>,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            world,
            access,
            confirmation_question,
            kind: ClientInvocationKind::Remote(payload),
        }
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world
    }

    /// What this invocation would do, as its own package classifies it. See
    /// [`ClientAccess`] for what a head may and may not conclude from it.
    pub fn access(&self) -> ClientAccess {
        self.access
    }

    /// The question declared at parse time, before any host is available.
    ///
    /// This is the floor, not the final prompt. A package that can say *what*
    /// it would destroy should resolve it through
    /// [`WorldClientPackage::confirmation`], which is what every client surface
    /// actually asks.
    pub fn confirmation_question(&self) -> Option<&str> {
        self.confirmation_question.as_deref()
    }

    pub fn kind(&self) -> &ClientInvocationKind {
        &self.kind
    }

    pub fn into_kind(self) -> ClientInvocationKind {
        self.kind
    }
}

pub type McpSchemaFactory = fn() -> Value;
pub type McpCallFactory = fn(Value) -> Result<ClientInvocation, Failure>;
pub type ReplyDecoder = fn(&Call, Reply) -> Result<Value, Failure>;
pub type WebParser = fn(Value) -> Result<ClientInvocation, Failure>;

/// Read a product-classified failure out of an answer that was *delivered*.
///
/// A World call answering `{"kind": "error", ...}` succeeded at every layer
/// below the product: the call was routed, authorized, executed, and replied
/// to. Only the product knows that value reports a failure, and only the
/// product knows which kind. A head that speaks a typed protocol — MCP error
/// classes, HTTP status — asks through here rather than pattern-matching a
/// schema it does not own.
///
/// Implementations must be a peek, not a decode. This runs on answers that are
/// fine, which is nearly all of them.
pub type FailureClassifier = fn(&Value) -> Option<(Failure, String)>;

/// One generic Space/host facility available to an application package.
///
/// These are deliberately product-neutral. A package may plan assignments or
/// implementation activation, but the Lait host remains the authority that
/// resolves the selected Orbit and commits the control operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostControlRequest {
    AssignmentList {
        actor: Option<String>,
    },
    AssignmentGrant {
        actor: String,
        assignments: Vec<HostAssignment>,
    },
    AssignmentRevoke {
        grant_id: String,
    },
    WorldActivate {
        world: WorldId,
    },
}

/// One content operation a package asks the shell to carry out.
///
/// The package names a file; the shell moves it. That split is the point. A
/// package that handled the bytes would need the control channel, a streaming
/// reader, and a ceiling — and would then be a second place where an attachment
/// can be truncated, buffered whole, or written somewhere nobody chose. Here it
/// says *what*, and the shell, which already owns transport, does the moving.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostContentRequest {
    /// Seal a local file onto the content plane. Answers `{content, size}`.
    Write { path: std::path::PathBuf },
    /// Save a committed content to a local path, streamed. Answers `{size}`.
    ///
    /// The path is the package's decision, because naming is product knowledge
    /// — see [`destination::sanitize_display_name`], which is how a peer-chosen
    /// name becomes one. The shell writes exactly where it is told.
    Read {
        content: String,
        destination: std::path::PathBuf,
    },
    /// What is known about one content, without moving it. Answers the same
    /// shape the web surface reports in headers.
    Stat { content: String },
}

/// One exact generic Mechanics assignment planned by a product package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostAssignment {
    pub world: String,
    pub capability: String,
    pub resource: Vec<String>,
}

/// Boxed future used to keep the host interface dyn-compatible.
pub type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 'a>>;

/// How many handles one decoration call may carry.
///
/// A board is one batch. Past this, the product is asking the identity plane
/// to name a list it should have already scoped.
pub const MAX_PRESENTATION_HANDLES: usize = 256;

/// A handle a product already had in a decoded reply. Not a Card, not
/// authority, and never a reason to place an Orbit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PresentationHandle {
    Device(String),
    Actor {
        /// When absent, the host fills in the Orbit it already authorized.
        space: Option<String>,
        actor: String,
    },
}

impl PresentationHandle {
    pub fn device(id: impl Into<String>) -> Self {
        Self::Device(id.into())
    }

    pub fn actor(actor: impl Into<String>) -> Self {
        Self::Actor {
            space: None,
            actor: actor.into(),
        }
    }

    /// Wire spelling the daemon's `BookResolve` accepts.
    pub fn to_wire(&self, default_space: Option<&str>) -> String {
        match self {
            Self::Device(id) => id.clone(),
            Self::Actor { space, actor } => {
                let space = space.as_deref().or(default_space).unwrap_or("");
                format!("actor:{space}:{actor}")
            }
        }
    }
}

/// One authored label for an exact requested handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationLabel {
    pub handle: PresentationHandle,
    /// Absent is "no Card", not an empty name.
    pub name: Option<String>,
}

/// Batched decoration. `coverage` is `Some("unavailable")` when the Orbit
/// could not be asked — which is not the same as "these people have no names".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationResolution {
    pub labels: Vec<PresentationLabel>,
    pub coverage: Option<String>,
}

impl PresentationResolution {
    pub fn unavailable() -> Self {
        Self {
            labels: Vec::new(),
            coverage: Some("unavailable".into()),
        }
    }

    pub fn is_unavailable(&self) -> bool {
        self.coverage.as_deref() == Some("unavailable")
    }

    pub fn name_for_actor(&self, actor: &str) -> Option<&str> {
        self.labels.iter().find_map(|label| match &label.handle {
            PresentationHandle::Actor { actor: id, .. } if id == actor => label.name.as_deref(),
            _ => None,
        })
    }
}

/// Facilities supplied by a trusted native client host to a World package.
///
/// The package owns orchestration and product semantics. The implementation
/// owns Orbit selection, daemon transport, acting identity, and generic Space
/// authority. `local_root` is caller-local state, never replicated World state.
pub trait ClientHost: Send + Sync {
    fn local_root(&self) -> &Path;
    fn call_world<'a>(&'a self, call: Call) -> ClientFuture<'a, Reply>;
    /// Evaluate a package-compiled query through the host's authenticated
    /// Runtime Find path. Hosts that are intentionally limited to legacy World
    /// projections may retain the typed refusal; native heads override this.
    fn call_find<'a>(
        &'a self,
        _world: WorldId,
        _query: runtime::find::Query,
    ) -> ClientFuture<'a, Value> {
        Box::pin(async {
            Err(Failure::new(
                "this client host does not expose Runtime Find",
            ))
        })
    }
    /// Inspect or request product-neutral durable Run lifecycle transitions.
    ///
    /// The DTO is owned by Runtime Exec. Packages remain responsible for the
    /// product vocabulary that decides when to call it, while the host binds
    /// the acting identity, Orbit, transport, and common Exec validator. The
    /// returned JSON is either a serialized `WorkReply` or the host's typed
    /// `{kind, error_kind, message}` refusal, so package routing preserves the
    /// reason a caller can act on.
    fn call_work<'a>(&'a self, request: runtime::exec::WorkRequest) -> ClientFuture<'a, Value>;
    fn call_control<'a>(&'a self, request: HostControlRequest) -> ClientFuture<'a, Value>;
    /// Move bytes on and off the content plane.
    ///
    /// Separate from [`Self::call_control`] because its answers are about a
    /// file rather than about Space authority, and because the shell streams
    /// here — the package never holds an attachment in memory, whatever its
    /// size.
    fn call_content<'a>(&'a self, request: HostContentRequest) -> ClientFuture<'a, Value>;
    /// Scoped, passive name decoration. Must never place an Orbit.
    fn call_identity<'a>(
        &'a self,
        handles: Vec<PresentationHandle>,
    ) -> ClientFuture<'a, PresentationResolution>;
}

/// Dynamic client behavior supplied by an independently launched World.
///
/// Metadata stays locally enumerable, while parsing and product semantics are
/// delegated to the exact runner generation that declared them.
pub trait ClientAdapter: Send + Sync {
    /// Map one World-owned document reference to its transient Body identity.
    fn transient_body(&self, document: &str) -> Result<[u8; 16], Failure>;
    fn parse_mcp(&self, tool: &str, input: Value) -> Result<ClientInvocation, Failure>;
    fn parse_web(&self, input: Value) -> Result<ClientInvocation, Failure>;
    fn classify_failure(&self, value: &Value) -> Option<(Failure, String)>;
    fn confirmation<'a>(
        &'a self,
        host: &'a dyn ClientHost,
        invocation: &'a ClientInvocation,
    ) -> ClientFuture<'a, Option<String>>;
    fn execute<'a>(
        &'a self,
        host: &'a dyn ClientHost,
        invocation: ClientInvocation,
    ) -> ClientFuture<'a, Value>;
}

pub type LocalInvocationHandler =
    for<'a> fn(&'a dyn ClientHost, LocalInvocation) -> ClientFuture<'a, Value>;
pub type TransientBodyResolver = fn(&str) -> Result<[u8; 16], Failure>;

/// Resolve the confirmation prompt for one invocation, with a host available.
///
/// A parse-time question can only name the *selector* the user typed, which for
/// a ref inferred from the working tree is unanswerable. This hook lets the
/// package read enough to name the thing itself before anyone is asked.
pub type ConfirmationResolver =
    for<'a> fn(&'a dyn ClientHost, &'a ClientInvocation) -> ClientFuture<'a, Option<String>>;

/// Decorate a decoded World reply with presentation labels.
///
/// Runs after decode, once, on the local HTTP/MCP response. It must not
/// change authoritative ids, and its output is never committed or cached
/// as World identity.
pub type ReplyDecorator =
    for<'a> fn(&'a dyn ClientHost, &'a Call, Value) -> ClientFuture<'a, Value>;

/// One product-local MCP tool. The registry prefixes `name` with the package's
/// mount, so independently developed Worlds cannot both publish a global `list`.
#[derive(Clone)]
pub struct McpTool {
    name: &'static str,
    description: &'static str,
    schema: McpSchema,
    call: McpCall,
}

#[derive(Clone)]
enum McpSchema {
    Local(McpSchemaFactory),
    Declared(Value),
}

#[derive(Clone)]
enum McpCall {
    Local(McpCallFactory),
    Remote(Arc<dyn ClientAdapter>),
}

impl McpTool {
    pub fn new(
        name: &'static str,
        description: &'static str,
        schema: McpSchemaFactory,
        call: McpCallFactory,
    ) -> Self {
        Self {
            name,
            description,
            schema: McpSchema::Local(schema),
            call: McpCall::Local(call),
        }
    }

    /// Construct one locally enumerable tool whose parser lives in a World
    /// process. Names and schemas are already held to the same package bounds
    /// by the remote declaration loader.
    pub fn remote(
        name: &'static str,
        description: &'static str,
        schema: Value,
        adapter: Arc<dyn ClientAdapter>,
    ) -> Self {
        Self {
            name,
            description,
            schema: McpSchema::Declared(schema),
            call: McpCall::Remote(adapter),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn schema(&self) -> Value {
        let mut value = match &self.schema {
            McpSchema::Local(factory) => factory(),
            McpSchema::Declared(value) => value.clone(),
        };
        // MCP requires a tool's input schema to declare `"type": "object"` at
        // the root. A serde-tagged union schemas as a bare `oneOf`/`anyOf` of
        // object variants — the root type is implied by every branch, but
        // clients validate the field itself and refuse the whole tool list
        // over its absence.
        if let Value::Object(root) = &mut value {
            if !root.contains_key("type")
                && (root.contains_key("oneOf") || root.contains_key("anyOf"))
            {
                root.insert("type".into(), Value::String("object".into()));
            }
        }
        value
    }

    pub fn call(&self, input: Value) -> Result<ClientInvocation, Failure> {
        match &self.call {
            McpCall::Local(call) => call(input),
            McpCall::Remote(adapter) => adapter.parse_mcp(self.name, input),
        }
    }
}

/// The agent-facing surface one World designed.
///
/// This is not a projection of the World's wire protocol. Tools are authored:
/// collapsed, split, retargeted, or omitted. [`Self::without`] names the
/// protocol commands that must never become tools, so a new `cmd` fails the
/// World's coverage test until someone decides.
pub struct AgentSurface {
    tools: Vec<McpTool>,
    instructions: &'static str,
    without: &'static [&'static str],
}

impl AgentSurface {
    /// A hand-designed surface. There is no constructor that builds this from
    /// a request enum — that path is what this type exists to refuse.
    pub fn designed(
        tools: Vec<McpTool>,
        instructions: &'static str,
        without: &'static [&'static str],
    ) -> Self {
        Self {
            tools,
            instructions,
            without,
        }
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    pub fn instructions(&self) -> &'static str {
        self.instructions
    }

    pub fn without(&self) -> &'static [&'static str] {
        self.without
    }
}

/// Gaps between a World's wire protocol and the agent surface it designed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageGaps {
    /// Protocol commands with neither a tool nor an explicit omission.
    pub missing: Vec<String>,
    /// `without` entries that are not protocol commands.
    pub stale_without: Vec<String>,
    /// `without` entries that a tool now emits.
    pub now_tooled: Vec<String>,
}

impl CoverageGaps {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.stale_without.is_empty() && self.now_tooled.is_empty()
    }

    pub fn check(self) -> Result<(), Failure> {
        if self.is_empty() {
            return Ok(());
        }
        let mut parts = Vec::new();
        if !self.missing.is_empty() {
            parts.push(format!(
                "commands with no tool and not listed as without: {:?}",
                self.missing
            ));
        }
        if !self.stale_without.is_empty() {
            parts.push(format!(
                "without entries that are not protocol commands: {:?}",
                self.stale_without
            ));
        }
        if !self.now_tooled.is_empty() {
            parts.push(format!(
                "without entries that now have a tool: {:?}",
                self.now_tooled
            ));
        }
        Err(Failure::new(parts.join("; ")))
    }
}

/// Every protocol command is either reachable through a designed tool or
/// written on `without`. A new `cmd` that nobody classified fails here.
pub fn agent_surface_coverage(
    defined: impl IntoIterator<Item = impl AsRef<str>>,
    reachable: impl IntoIterator<Item = impl AsRef<str>>,
    without: &[&str],
) -> CoverageGaps {
    let defined: BTreeSet<String> = defined
        .into_iter()
        .map(|tag| tag.as_ref().to_owned())
        .collect();
    let reachable: BTreeSet<String> = reachable
        .into_iter()
        .map(|tag| tag.as_ref().to_owned())
        .collect();
    let without_set: BTreeSet<&str> = without.iter().copied().collect();
    CoverageGaps {
        missing: defined
            .iter()
            .filter(|tag| !reachable.contains(*tag) && !without_set.contains(tag.as_str()))
            .cloned()
            .collect(),
        stale_without: without
            .iter()
            .filter(|tag| !defined.iter().any(|command| command == *tag))
            .map(|tag| (*tag).to_owned())
            .collect(),
        now_tooled: without
            .iter()
            .copied()
            .filter(|tag| reachable.contains(*tag))
            .map(str::to_owned)
            .collect(),
    }
}

/// The client interfaces shipped by one World application package.
#[derive(Clone)]
pub struct WorldClientPackage {
    world: WorldId,
    /// Where this package is actually mounted.
    ///
    /// Owned, and not the `&'static str` a World compiles in, because the
    /// World declares a *preference* and the host decides where it lands. For
    /// a released World those are the same string and the published-API
    /// guarantee on `MOUNT` is untouched. A local World — a tree somebody is
    /// working on — is assigned one in its own namespace, so it can run beside
    /// the release it was copied from without either one answering to the
    /// other's tools or URLs.
    mount: String,
    mcp_tools: Vec<McpTool>,
    mcp_instructions: &'static str,
    without: &'static [&'static str],
    decode_reply: ReplyDecoder,
    classify_failure: Option<FailureClassifier>,
    local_handler: Option<LocalInvocationHandler>,
    web_parser: Option<WebParser>,
    confirmation: Option<ConfirmationResolver>,
    decorator: Option<ReplyDecorator>,
    transient_body: Option<TransientBodyResolver>,
    adapter: Option<Arc<dyn ClientAdapter>>,
    display: Display,
    display_surfaces: BTreeMap<String, display::DisplaySurface>,
}

/// What a client needs to draw a World as a row somebody can open.
///
/// Separate from the mount, and deliberately. A mount is a namespace key that
/// prefixes tool names and route segments — it is published, it is machine
/// input, and it must never change. A display name is for a person to read, and
/// changing it breaks nothing. A seam that made one do both work would have to
/// treat every rename as a compatibility event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    /// What to call this World in a list. Falls back to the mount, which is at
    /// least a real name rather than an id, but a World that means to be listed
    /// should say what it is called.
    name: &'static str,
    /// A short glyph a row can draw without loading anything. Deliberately not
    /// an image path: a Library that fetched an asset per row to draw itself
    /// would make listing cost what opening costs, which is the same defect as
    /// mounting a Station to list it.
    icon: Option<&'static str>,
    /// The path `Open` lands on, relative to the head's root for this World.
    /// `None` means this World is not openable in a browser, which is a real
    /// answer and not a missing one.
    entry_path: Option<&'static str>,
    /// One line saying what this World is for, drawn under the name.
    ///
    /// A line, not a paragraph: it sits under a title in a list and in a detail
    /// pane, and a World that needs three sentences to say what it is has a
    /// naming problem rather than a space problem.
    tagline: Option<&'static str>,
    /// The colour this World is drawn from, packed `0xRRGGBB`.
    ///
    /// A *seed*, not an asset, for exactly the reason [`Display::icon`] is a
    /// glyph rather than an image path: a Library that fetched a banner per row
    /// to draw itself would make listing cost what opening costs. A client
    /// derives whatever it needs from this one number, and derives it locally.
    accent: Option<u32>,
    /// Named places inside this World somebody can go straight to.
    ///
    /// The World declares them because the World owns its own URL grammar; the
    /// client knows only which Orbit it is opening. A path may carry the single
    /// placeholder [`SPACE_PLACEHOLDER`], which the client replaces with the
    /// space it is opening. That is the whole of the coupling, and it runs in
    /// the direction that keeps a World's routes the World's business.
    routes: &'static [Route],
    /// The square mark drawn where a World is one row in a list — bounded PNG
    /// bytes loaded from the selected release.
    ///
    /// **Bytes, never a path.** The rule [`Display::icon`] states still holds:
    /// a Library that went to a network per row to draw itself would make
    /// listing cost what opening costs. The installer verifies the bytes once;
    /// the client adapter retains them for the selected generation.
    mark: Option<&'static [u8]>,
    /// The frame drawn behind a World's title on a detail surface — bounded
    /// PNG bytes under the same rule as [`Display::mark`].
    ///
    /// Separate from the mark because they are drawn at sizes an order apart. A
    /// mark at 24 pixels and a banner at 200 cannot be the same image without
    /// one of them being wrong: detail that reads at 200 is mud at 24, and art
    /// composed for 24 is four bland shapes at 200.
    hero: Option<&'static [u8]>,
}

/// The most one artwork may weigh.
///
/// It is carried by every installation of the World. Generous enough for a mark
/// and a banner at the sizes they are drawn; far below anything that would be
/// called a wallpaper.
pub const MAX_ARTWORK_BYTES: usize = 256 * 1024;

/// The widest an artwork may be.
///
/// Both are drawn square and scaled down by the client. Past this a World is
/// paying binary size for pixels no surface asks for.
pub const MAX_ARTWORK_SIDE: u32 = 512;

/// One named place inside a World.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    label: &'static str,
    path: &'static str,
}

impl Route {
    pub const fn new(label: &'static str, path: &'static str) -> Self {
        Self { label, path }
    }

    pub const fn label(&self) -> &'static str {
        self.label
    }

    /// The declared path, with [`SPACE_PLACEHOLDER`] still in it.
    pub const fn path(&self) -> &'static str {
        self.path
    }

    /// The path for one space.
    pub fn resolve(&self, space: &str) -> String {
        self.path.replace(SPACE_PLACEHOLDER, space)
    }
}

/// The one substitution a declared route may ask for.
pub const SPACE_PLACEHOLDER: &str = "{space}";

/// How many routes a World may declare.
///
/// A strip of places, not a menu. Past this, a client is drawing navigation the
/// World should be drawing on its own page, where it has the room.
pub const MAX_ROUTES: usize = 8;

impl Display {
    /// The honest default for a World that has not said: named by its mount,
    /// no icon, and *not openable* — because guessing an entry path produces a
    /// row whose button leads somewhere nobody chose.
    pub const fn unstated(mount: &'static str) -> Self {
        Self {
            name: mount,
            icon: None,
            entry_path: None,
            tagline: None,
            accent: None,
            routes: &[],
            mark: None,
            hero: None,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn icon(&self) -> Option<&'static str> {
        self.icon
    }

    pub const fn entry_path(&self) -> Option<&'static str> {
        self.entry_path
    }

    pub const fn tagline(&self) -> Option<&'static str> {
        self.tagline
    }

    pub const fn accent(&self) -> Option<u32> {
        self.accent
    }

    pub const fn routes(&self) -> &'static [Route] {
        self.routes
    }

    pub const fn mark(&self) -> Option<&'static [u8]> {
        self.mark
    }

    pub const fn hero(&self) -> Option<&'static [u8]> {
        self.hero
    }
}

/// The eight bytes every PNG starts with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Read the dimensions out of a PNG's header.
///
/// The signature is 8 bytes, then a chunk length, then `IHDR`, then width and
/// height as big-endian `u32`s — so the answer is 24 bytes in, with no decoder
/// and no dependency. `None` for anything that is not a PNG whose first chunk
/// is `IHDR`, which the spec requires it to be.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != PNG_SIGNATURE.as_slice() || bytes.get(12..16)? != b"IHDR".as_slice() {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

/// Hold one declared artwork to the bounds a client draws it within.
///
/// Local/embedded packages validate here; process releases validate the same
/// bounds during staging before the selected generation is launched.
fn validate_artwork(world: &WorldId, kind: &str, bytes: &[u8]) -> Result<(), Failure> {
    artwork_bounds(kind, bytes).map_err(|why| Failure::new(format!("World '{world}' {why}")))
}

/// The bounds every artwork is held to, wherever it came from.
///
/// Every World meets these when its package is built or its release is staged.
/// One function keeps local test adapters and process releases under one rule.
pub fn artwork_bounds(kind: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err(format!("declares an empty {kind}"));
    }
    if bytes.len() > MAX_ARTWORK_BYTES {
        return Err(format!(
            "declares a {kind} of {} bytes; an artwork stops at {MAX_ARTWORK_BYTES}",
            bytes.len()
        ));
    }
    let Some((width, height)) = png_dimensions(bytes) else {
        return Err(format!("declares a {kind} that is not a PNG"));
    };
    // Square, because every surface that draws one draws it in a square: a
    // 3:1 banner in a mark's plate is either stretched or cropped, and which
    // of the two happens would be the client deciding what the World meant.
    if width != height {
        return Err(format!(
            "declares a {width}×{height} {kind}; an artwork is square"
        ));
    }
    if width > MAX_ARTWORK_SIDE {
        return Err(format!(
            "declares a {kind} {width} wide; an artwork stops at {MAX_ARTWORK_SIDE}"
        ));
    }
    Ok(())
}

impl WorldClientPackage {
    /// Declare one package under `mount`, the single namespace key every head
    /// addresses it by.
    ///
    /// `mount` is not decoration. It prefixes every public MCP tool name and it
    /// is the `{world}` segment of the HTTP RPC route, so changing it renames
    /// every tool an agent has learned and breaks every URL a head has built.
    /// Treat it as published.
    ///
    /// The agent surface is designed, not generated from the wire protocol.
    pub fn new(
        world: WorldId,
        mount: &'static str,
        surface: AgentSurface,
        decode_reply: ReplyDecoder,
    ) -> Result<Self, Failure> {
        validate_name("mount", mount)?;
        let mut local_tools = BTreeSet::new();
        for tool in surface.tools() {
            validate_name("MCP tool", tool.name())?;
            if !local_tools.insert(tool.name()) {
                return Err(Failure::new(format!(
                    "World '{}' declares duplicate MCP tool '{}'",
                    world,
                    tool.name()
                )));
            }
        }
        Ok(Self {
            world,
            mount: mount.to_owned(),
            mcp_tools: surface.tools,
            mcp_instructions: surface.instructions,
            without: surface.without,
            decode_reply,
            classify_failure: None,
            local_handler: None,
            web_parser: None,
            confirmation: None,
            decorator: None,
            transient_body: None,
            adapter: None,
            display: Display::unstated(mount),
            display_surfaces: BTreeMap::new(),
        })
    }

    /// Register one package-owned display projection. Its runtime
    /// implementation and semantic descriptor are frozen at composition time,
    /// before a coordinator can create an assignment.
    pub fn with_display_surface(
        mut self,
        surface: display::DisplaySurface,
    ) -> Result<Self, Failure> {
        surface.descriptor.validate(&self.world)?;
        let id = surface.descriptor.id.as_str().to_string();
        if self.display_surfaces.contains_key(&id) {
            return Err(Failure::new(format!(
                "World '{}' declares duplicate display surface '{id}'",
                self.world
            )));
        }
        self.display_surfaces.insert(id, surface);
        Ok(self)
    }

    /// Say how this World should be drawn and where `Open` lands.
    ///
    /// A World that does not call this is listed by its mount and is *not*
    /// openable — see [`Display::unstated`]. That is the honest default: an
    /// entry path invented on a World's behalf is a button that leads somewhere
    /// nobody chose.
    pub fn with_display(
        mut self,
        name: &'static str,
        icon: Option<&'static str>,
        entry_path: Option<&'static str>,
    ) -> Result<Self, Failure> {
        if name.trim().is_empty() {
            return Err(Failure::new(format!(
                "World '{}' declares an empty display name",
                self.world
            )));
        }
        // An entry path is joined onto a head's root, so a relative or
        // traversing one would resolve somewhere the World does not own.
        if let Some(path) = entry_path {
            if !path.starts_with('/') || path.contains("..") {
                return Err(Failure::new(format!(
                    "World '{}' declares entry path '{path}', which must be absolute                      and must not traverse",
                    self.world
                )));
            }
        }
        self.display = Display {
            name,
            icon,
            entry_path,
            ..self.display
        };
        Ok(self)
    }

    /// Say, in one line, what this World is for.
    pub fn with_tagline(mut self, tagline: &'static str) -> Result<Self, Failure> {
        let trimmed = tagline.trim();
        if trimmed.is_empty() {
            return Err(Failure::new(format!(
                "World '{}' declares an empty tagline",
                self.world
            )));
        }
        // The bound is here rather than in a client, because a client that had
        // to elide would be deciding what a World meant to say.
        const LONGEST: usize = 96;
        if trimmed.chars().count() > LONGEST {
            return Err(Failure::new(format!(
                "World '{}' declares a tagline of {} characters; a tagline is one line and stops at {LONGEST}",
                self.world,
                trimmed.chars().count()
            )));
        }
        self.display = Display {
            tagline: Some(tagline),
            ..self.display
        };
        Ok(self)
    }

    /// Say which colour this World is drawn from, packed `0xRRGGBB`.
    pub fn with_accent(mut self, accent: u32) -> Result<Self, Failure> {
        if accent > 0x00FF_FFFF {
            return Err(Failure::new(format!(
                "World '{}' declares accent {accent:#08x}, which is not a 24-bit colour",
                self.world
            )));
        }
        self.display = Display {
            accent: Some(accent),
            ..self.display
        };
        Ok(self)
    }

    /// Ship this World's own artwork: a square mark for a row, a square frame
    /// for a detail surface.
    ///
    /// Both are already-verified PNG bytes retained for this client generation.
    /// Process-backed adapters load them from the selected immutable release;
    /// local embedders may supply static bytes. A World may declare either,
    /// both, or neither; a client that is given neither draws what it can derive from
    /// [`Display::accent`], which is why no default artwork exists here.
    pub fn with_artwork(
        mut self,
        mark: Option<&'static [u8]>,
        hero: Option<&'static [u8]>,
    ) -> Result<Self, Failure> {
        if let Some(bytes) = mark {
            validate_artwork(&self.world, "mark", bytes)?;
        }
        if let Some(bytes) = hero {
            validate_artwork(&self.world, "hero", bytes)?;
        }
        self.display = Display {
            mark,
            hero,
            ..self.display
        };
        Ok(self)
    }

    /// Say which places inside this World somebody can go straight to.
    pub fn with_routes(mut self, routes: &'static [Route]) -> Result<Self, Failure> {
        if routes.len() > MAX_ROUTES {
            return Err(Failure::new(format!(
                "World '{}' declares {} routes; a strip of places stops at {MAX_ROUTES}",
                self.world,
                routes.len()
            )));
        }
        let mut seen = BTreeSet::new();
        for route in routes {
            if route.label.trim().is_empty() {
                return Err(Failure::new(format!(
                    "World '{}' declares a route with no label",
                    self.world
                )));
            }
            if !seen.insert(route.label) {
                return Err(Failure::new(format!(
                    "World '{}' declares two routes labelled '{}'",
                    self.world, route.label
                )));
            }
            // The rule an entry path is held to, for the same reason: a route
            // is joined onto a head's root, so a relative or traversing one
            // resolves somewhere the World does not own.
            if !route.path.starts_with('/') || route.path.contains("..") {
                return Err(Failure::new(format!(
                    "World '{}' declares route path '{}', which must be absolute and must not traverse",
                    self.world, route.path
                )));
            }
            // One placeholder, spelled one way. A path carrying any other brace
            // expression is a World expecting a substitution no client makes,
            // which opens a URL with a literal brace in it.
            let without = route.path.replace(SPACE_PLACEHOLDER, "");
            if without.contains('{') || without.contains('}') {
                return Err(Failure::new(format!(
                    "World '{}' declares route path '{}' with a placeholder other than {SPACE_PLACEHOLDER}",
                    self.world, route.path
                )));
            }
        }
        self.display = Display {
            routes,
            ..self.display
        };
        Ok(self)
    }

    /// How a client should draw this World.
    pub const fn display(&self) -> &Display {
        &self.display
    }

    pub fn display_surfaces(&self) -> impl Iterator<Item = &display::DisplaySurface> {
        self.display_surfaces.values()
    }

    pub fn display_surface(
        &self,
        id: &display::DisplaySurfaceId,
    ) -> Option<&display::DisplaySurface> {
        self.display_surfaces.get(id.as_str())
    }

    pub fn with_failure_classifier(mut self, classifier: FailureClassifier) -> Self {
        self.classify_failure = Some(classifier);
        self
    }

    pub fn with_local_handler(mut self, handler: LocalInvocationHandler) -> Self {
        self.local_handler = Some(handler);
        self
    }

    pub fn with_web_parser(mut self, parser: WebParser) -> Self {
        self.web_parser = Some(parser);
        self
    }

    pub fn with_confirmation(mut self, confirmation: ConfirmationResolver) -> Self {
        self.confirmation = Some(confirmation);
        self
    }

    /// Decorate decoded replies with identity-scoped presentation labels.
    ///
    /// Packages without a decorator keep byte-for-byte-equivalent decoded
    /// JSON. The callback understands its own schema; this crate does not.
    pub fn with_decorator(mut self, decorator: ReplyDecorator) -> Self {
        self.decorator = Some(decorator);
        self
    }

    /// Supply the World's one-way document-reference to Body mapping used by
    /// the product-neutral transient presence plane.
    pub fn with_transient_body(mut self, resolver: TransientBodyResolver) -> Self {
        self.transient_body = Some(resolver);
        self
    }

    /// Delegate parsing, confirmation, and execution to an independently
    /// launched World generation.
    pub fn with_client_adapter(mut self, adapter: Arc<dyn ClientAdapter>) -> Self {
        self.adapter = Some(adapter);
        self
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn transient_body(&self, document: &str) -> Result<[u8; 16], Failure> {
        if let Some(adapter) = self.adapter.as_deref() {
            return adapter.transient_body(document);
        }
        self.transient_body.ok_or_else(|| {
            Failure::new(format!(
                "World '{}' exposes no transient Body mapping",
                self.world
            ))
        })?(document)
    }

    /// The namespace key every head addresses this package by — the MCP tool
    /// prefix and the `{world}` route segment. See [`Self::new`].
    pub fn mount(&self) -> &str {
        &self.mount
    }

    /// Mount this package somewhere other than where it asked to be.
    ///
    /// The World declares a preference; the host decides. Used to put a local
    /// World — a tree somebody is working on — in its own namespace so it can
    /// run beside the release it was copied from. Never used for a released
    /// World, whose `MOUNT` is published API precisely so that it does not
    /// move.
    pub fn mounted_at(mut self, mount: impl Into<String>) -> Self {
        self.mount = mount.into();
        self
    }

    pub fn mcp_tools(&self) -> &[McpTool] {
        &self.mcp_tools
    }

    pub fn mcp_instructions(&self) -> &'static str {
        self.mcp_instructions
    }

    /// Protocol commands this World designed out of the agent surface.
    pub fn without(&self) -> &'static [&'static str] {
        self.without
    }

    /// Refuse when this package's public MCP names collide with a host's own.
    ///
    /// Composition of one pin is the only honest place to find this: two tools
    /// with one public name means whichever the router registered last silently
    /// wins. Checking a World that is not on this session would empty the
    /// session for a collision nobody can reach.
    pub fn validate_reserved<'a>(
        &self,
        reserved_mcp: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), Failure> {
        let reserved_mcp: BTreeSet<_> = reserved_mcp.into_iter().collect();
        for mounted in mounted_tools(self) {
            if reserved_mcp.contains(mounted.public_name.as_str()) {
                return Err(Failure::new(format!(
                    "World '{}' MCP tool '{}' collides with a shell tool",
                    self.world, mounted.public_name
                )));
            }
        }
        Ok(())
    }

    pub fn decode_reply(&self, call: &Call, reply: Reply) -> Result<Value, Failure> {
        (self.decode_reply)(call, reply)
    }

    /// Ask the package whether a delivered answer reports a failure, and of
    /// what class. See [`FailureClassifier`].
    pub fn classify_failure(&self, value: &Value) -> Option<(Failure, String)> {
        self.adapter
            .as_deref()
            .and_then(|adapter| adapter.classify_failure(value))
            .or_else(|| self.classify_failure.and_then(|classify| classify(value)))
    }

    pub fn parse_web(&self, input: Value) -> Result<ClientInvocation, Failure> {
        if let Some(adapter) = self.adapter.as_deref() {
            let invocation = adapter.parse_web(input)?;
            self.validate_invocation(&invocation)?;
            return Ok(invocation);
        }
        let parser = self.web_parser.ok_or_else(|| {
            Failure::new(format!(
                "World '{}' does not expose a web client interface",
                self.world
            ))
        })?;
        let invocation = parser(input)?;
        self.validate_invocation(&invocation)?;
        Ok(invocation)
    }

    /// The prompt a client must show before running `invocation`, or `None`
    /// when the package considers it unremarkable.
    ///
    /// Every surface asks through here so the CLI prompt and the browser's
    /// modal cannot disagree about what is dangerous, or describe it
    /// differently. A package without a resolver gets its declared question
    /// verbatim; a resolver that fails falls back to it rather than blocking a
    /// confirmation on a lookup that was only ever there to add detail.
    pub fn confirmation<'a>(
        &'a self,
        host: &'a dyn ClientHost,
        invocation: &'a ClientInvocation,
    ) -> ClientFuture<'a, Option<String>> {
        Box::pin(async move {
            self.validate_invocation(invocation)?;
            if let Some(adapter) = self.adapter.as_deref() {
                return adapter.confirmation(host, invocation).await;
            }
            let declared = invocation.confirmation_question().map(str::to_string);
            let Some(resolver) = self.confirmation else {
                return Ok(declared);
            };
            Ok(resolver(host, invocation).await.unwrap_or(declared))
        })
    }

    /// Execute one invocation through its owning package and answer with the
    /// decoded value — one decode per call, then optional presentation.
    ///
    /// It used to cost three. Every World call cloned the decoded value, handed
    /// the clone to a product presenter that parsed it back into a typed
    /// response, and re-serialized that response into a `String` — a full
    /// deep-copy, decode, and encode of the whole payload. Every head then
    /// dropped the string: the HTTP surface returned `value` and never looked
    /// at it, and MCP re-encoded `value` itself. So a board with a thousand rows
    /// paid three passes over a thousand rows to produce text nobody read.
    ///
    /// The presenter is gone and a head renders from the value it is handed.
    /// This is the World-call path; work removed here is removed from every
    /// request the product serves.
    pub fn execute<'a>(
        &'a self,
        host: &'a dyn ClientHost,
        invocation: ClientInvocation,
    ) -> ClientFuture<'a, Value> {
        Box::pin(async move {
            self.validate_invocation(&invocation)?;
            if let Some(adapter) = self.adapter.as_deref() {
                return adapter.execute(host, invocation).await;
            }
            match invocation.into_kind() {
                ClientInvocationKind::World(call) => {
                    let reply = host.call_world(call.clone()).await?;
                    let decoded = self.decode_reply(&call, reply)?;
                    let Some(decorate) = self.decorator else {
                        return Ok(decoded);
                    };
                    // Presentation must not fail the product: a book that
                    // could not be asked is an absence of names, not a
                    // failed World call.
                    Ok(decorate(host, &call, decoded.clone())
                        .await
                        .unwrap_or(decoded))
                }
                ClientInvocationKind::Find { query, presenter } => {
                    let value = host.call_find(self.world.clone(), query).await?;
                    let Some(present) = presenter else {
                        return Ok(value);
                    };
                    let answer = serde_json::from_value(value).map_err(|error| {
                        Failure::new(format!(
                            "host returned an invalid Runtime Find answer: {error}"
                        ))
                    })?;
                    present(answer)
                }
                ClientInvocationKind::Local(local) => {
                    let handler = self.local_handler.ok_or_else(|| {
                        Failure::new(format!(
                            "World '{}' does not expose local client operations",
                            self.world
                        ))
                    })?;
                    handler(host, local).await
                }
                ClientInvocationKind::Remote(_) => Err(Failure::new(
                    "an opaque invocation has no process-backed client adapter",
                )),
            }
        })
    }

    pub fn validate_invocation(&self, invocation: &ClientInvocation) -> Result<(), Failure> {
        if invocation.world_id() == &self.world {
            Ok(())
        } else {
            Err(Failure::new(format!(
                "World '{}' client package cannot execute an invocation for '{}'",
                self.world,
                invocation.world_id()
            )))
        }
    }
}

/// A mounted tool with its collision-safe public name.
pub struct MountedMcpTool<'a> {
    pub world: &'a WorldId,
    pub public_name: String,
    pub tool: &'a McpTool,
}

/// Compile-time composition of every client-facing World package.
#[derive(Clone, Default)]
pub struct WorldClientRegistry {
    packages: BTreeMap<String, WorldClientPackage>,
    mounts: BTreeMap<String, String>,
}

impl WorldClientRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_package(mut self, package: WorldClientPackage) -> Result<Self, Failure> {
        let world = package.world().as_str().to_string();
        if self.packages.contains_key(&world) {
            return Err(Failure::new(format!(
                "duplicate client package for World '{world}'"
            )));
        }
        if let Some(existing) = self.mounts.get(package.mount()) {
            return Err(Failure::new(format!(
                "mount '{}' is claimed by Worlds '{}' and '{}'",
                package.mount(),
                existing,
                world
            )));
        }
        self.mounts
            .insert(package.mount().to_owned(), world.clone());
        self.packages.insert(world, package);
        Ok(self)
    }

    /// Refuse a build whose product tool names collide with a host's own.
    ///
    /// Composition time is the only honest place to find this: two tools with
    /// one public name means whichever the router registered last silently wins,
    /// and an agent calling the loser gets the winner's behavior.
    pub fn validate_reserved<'a>(
        &self,
        reserved_mcp: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), Failure> {
        let reserved_mcp: BTreeSet<_> = reserved_mcp.into_iter().collect();
        for package in self.packages.values() {
            package.validate_reserved(reserved_mcp.iter().copied())?;
        }
        Ok(())
    }

    pub fn packages(&self) -> impl Iterator<Item = &WorldClientPackage> {
        self.packages.values()
    }

    pub fn package_for_mount(&self, mount: &str) -> Option<&WorldClientPackage> {
        let world = self.mounts.get(mount)?;
        self.packages.get(world)
    }

    pub fn package_for_world(&self, world: &WorldId) -> Option<&WorldClientPackage> {
        self.packages.get(world.as_str())
    }

    pub fn mcp_tools(&self) -> impl Iterator<Item = MountedMcpTool<'_>> {
        self.packages.values().flat_map(mounted_tools)
    }

    /// The one World this MCP session may speak.
    ///
    /// Unset + one package is the sole-World default. Unset + many is a
    /// refusal that names `LAIT_WORLD`. A named mount with no selected
    /// installation is a refusal, not a silent empty tool list.
    pub fn pin(&self, requested: Option<&str>) -> Result<&WorldClientPackage, Failure> {
        if let Some(mount) = requested {
            return self.package_for_mount(mount).ok_or_else(|| {
                let selected = self.selected_mounts();
                Failure::new(format!(
                    "LAIT_WORLD={mount} has no selected installation. Selected mounts: {selected}"
                ))
            });
        }
        let mut packages: Vec<_> = self.packages().collect();
        match packages.len() {
            0 => Err(Failure::new(
                "this identity has no selected World MCP surface",
            )),
            1 => Ok(packages.remove(0)),
            _ => {
                let selected = self.selected_mounts();
                Err(Failure::new(format!(
                    "LAIT_WORLD is unset and this identity has more than one selected World ({selected}); \
                     set LAIT_WORLD to one mount"
                )))
            }
        }
    }

    fn selected_mounts(&self) -> String {
        let mounts: Vec<_> = self.packages().map(WorldClientPackage::mount).collect();
        if mounts.is_empty() {
            "(none)".into()
        } else {
            mounts.join(", ")
        }
    }
}

fn mounted_tools(package: &WorldClientPackage) -> impl Iterator<Item = MountedMcpTool<'_>> {
    package.mcp_tools().iter().map(|tool| MountedMcpTool {
        world: package.world(),
        public_name: format!("{}_{}", package.mount(), tool.name()),
        tool,
    })
}

fn validate_name(kind: &str, name: &str) -> Result<(), Failure> {
    let valid = !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(Failure::new(format!(
            "{kind} '{name}' must use lowercase ASCII letters, digits, '_' or '-'"
        )))
    }
}

#[cfg(test)]
mod tests {
    /// A World that says nothing is named by its mount and is *not* openable.
    /// The alternative — defaulting the entry path to `/` — produces a row
    /// whose button leads somewhere nobody chose, and nothing downstream can
    /// tell that apart from a declared route.
    #[test]
    fn a_world_that_declares_no_display_is_named_by_its_mount_and_is_not_openable() {
        let display = super::Display::unstated("issues");
        assert_eq!(display.name(), "issues");
        assert_eq!(display.icon(), None);
        assert_eq!(
            display.entry_path(),
            None,
            "an entry path was invented for a World that declared none"
        );
    }

    use super::*;

    fn empty_schema() -> Value {
        serde_json::json!({"type": "object", "additionalProperties": false})
    }

    fn tagged_union_schema() -> Value {
        serde_json::json!({
            "oneOf": [
                {"type": "object", "properties": {"action": {"const": "a"}}},
                {"type": "object", "properties": {"action": {"const": "b"}}},
            ]
        })
    }

    fn refuse_call(_: Value) -> Result<ClientInvocation, Failure> {
        Err(Failure::new("unused"))
    }

    /// MCP clients validate `inputSchema.type == "object"` on every tool and
    /// refuse the whole tool list when one fails. A serde-tagged union schemas
    /// as a bare `oneOf` of object variants, which is exactly that failure.
    #[test]
    fn a_tagged_union_tool_schema_declares_its_implied_object_type() {
        let tool = McpTool::new("work", "lifecycle", tagged_union_schema, refuse_call);
        let schema = tool.schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["oneOf"].is_array(), "the union itself must survive");

        let plain = McpTool::new("view", "read", empty_schema, refuse_call);
        assert_eq!(
            plain.schema(),
            empty_schema(),
            "object schemas pass through"
        );
    }

    fn files_call(_: Value) -> Result<Call, Failure> {
        Call::new(
            WorldId::parse("com.example.files").unwrap(),
            "files.list",
            1,
            vec![],
        )
        .map_err(|error| Failure::new(error.to_string()))
    }

    fn files_invocation(input: Value) -> Result<ClientInvocation, Failure> {
        files_call(input).map(|call| ClientInvocation::world(call, ClientAccess::Query, None))
    }

    fn notes_call(_: Value) -> Result<Call, Failure> {
        Call::new(
            WorldId::parse("com.example.notes").unwrap(),
            "notes.list",
            1,
            vec![],
        )
        .map_err(|error| Failure::new(error.to_string()))
    }

    fn notes_invocation(input: Value) -> Result<ClientInvocation, Failure> {
        notes_call(input).map(|call| ClientInvocation::world(call, ClientAccess::Query, None))
    }

    fn decode_json_reply(call: &Call, reply: Reply) -> Result<Value, Failure> {
        reply
            .validate_for(call)
            .map_err(|error| Failure::new(error.to_string()))?;
        let payload = reply
            .into_result()
            .map_err(|error| Failure::new(error.to_string()))?;
        serde_json::from_slice(&payload)
            .map_err(|error| Failure::new(format!("decode reply: {error}")))
    }

    fn package(world: &str, mount: &'static str) -> WorldClientPackage {
        let call = if mount == "notes" {
            notes_invocation as McpCallFactory
        } else {
            files_invocation as McpCallFactory
        };
        WorldClientPackage::new(
            WorldId::parse(world).unwrap(),
            mount,
            AgentSurface::designed(
                vec![McpTool::new("list", "List objects.", empty_schema, call)],
                "Work with files.",
                &[],
            ),
            decode_json_reply,
        )
        .unwrap()
    }

    /// A square PNG of `side`, headed exactly as the spec requires — the
    /// bytes `png_dimensions` reads, and nothing past them.
    fn png(side: u32) -> Vec<u8> {
        let mut bytes = Vec::from(PNG_SIGNATURE);
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&side.to_be_bytes());
        bytes.extend_from_slice(&side.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    /// Artwork is `&'static [u8]` because a real one is `include_bytes!`. A
    /// test builds its bytes at runtime, so it leaks them — the lifetime is
    /// the declaration's whole point and is not worth loosening to test it.
    fn fixed(bytes: Vec<u8>) -> &'static [u8] {
        Box::leak(bytes.into_boxed_slice())
    }

    /// Artwork is optional in both halves, and a World that ships none is a
    /// World drawn from its accent — not a client with a missing file.
    #[test]
    fn a_world_that_ships_no_artwork_declares_none() {
        let display = super::Display::unstated("issues");
        assert_eq!(display.mark(), None);
        assert_eq!(display.hero(), None);

        let package = package("com.example.files", "files")
            .with_artwork(None, None)
            .expect("no artwork is a legal declaration");
        assert_eq!(package.display().mark(), None);
        assert_eq!(package.display().hero(), None);
    }

    /// The bounds are the whole point of taking bytes instead of a path: the
    /// artwork ships inside every independently installed World release, so a
    /// World that hands over a photograph is spending every install's bytes.
    /// Each of these is refused where the person who can fix the image stands.
    #[test]
    fn artwork_is_held_to_its_bounds_at_declaration() {
        let square = fixed(png(64));
        assert!(package("com.example.files", "files")
            .with_artwork(Some(square), Some(square))
            .is_ok());

        // Not a PNG at all.
        let not_png = fixed(b"GIF89a and then some bytes to clear the length floor".to_vec());
        assert!(package("com.example.files", "files")
            .with_artwork(Some(not_png), None)
            .is_err());

        // Empty is absence spelled wrong: `None` says it.
        assert!(package("com.example.files", "files")
            .with_artwork(Some(&[]), None)
            .is_err());

        // Oblong: every surface draws these in a square, and a client that
        // cropped or stretched would be deciding what the World meant.
        let mut oblong = png(64);
        oblong[20..24].copy_from_slice(&32u32.to_be_bytes());
        assert!(package("com.example.files", "files")
            .with_artwork(None, Some(fixed(oblong)))
            .is_err());

        // Wider than anything asks for.
        let huge = fixed(png(MAX_ARTWORK_SIDE + 1));
        assert!(package("com.example.files", "files")
            .with_artwork(None, Some(huge))
            .is_err());

        // Heavier than a client should carry. A valid header, so this can only
        // be the weight being caught.
        let mut heavy = png(64);
        heavy.resize(MAX_ARTWORK_BYTES + 1, 0);
        assert!(package("com.example.files", "files")
            .with_artwork(Some(fixed(heavy)), None)
            .is_err());
    }

    /// The header read is the whole of the decoding done here, so it has to be
    /// right about the two numbers it takes.
    #[test]
    fn png_dimensions_are_read_from_the_header_alone() {
        assert_eq!(super::png_dimensions(&png(196)), Some((196, 196)));
        assert_eq!(super::png_dimensions(b"short"), None);
        let mut wrong_chunk = png(64);
        wrong_chunk[12..16].copy_from_slice(b"IDAT");
        assert_eq!(super::png_dimensions(&wrong_chunk), None);
    }

    #[test]
    fn a_second_world_mounts_without_host_specific_code() {
        let registry = WorldClientRegistry::new()
            .with_package(package("com.example.files", "files"))
            .unwrap()
            .with_package(package("com.example.notes", "notes"))
            .unwrap();
        let mounts: Vec<_> = registry.packages().map(WorldClientPackage::mount).collect();
        assert_eq!(mounts, vec!["files", "notes"]);
        // The mount is the tool namespace: two independently written Worlds
        // both publish `list`, and neither shadows the other.
        let tools: Vec<_> = registry.mcp_tools().map(|tool| tool.public_name).collect();
        assert_eq!(tools, vec!["files_list", "notes_list"]);
        // And it is the route key a head resolves a request path through.
        assert_eq!(
            registry
                .package_for_mount("notes")
                .unwrap()
                .world()
                .as_str(),
            "com.example.notes"
        );
        assert!(registry.package_for_mount("ledger").is_none());

        let call = files_call(Value::Null).unwrap();
        let reply = Reply::ok(
            &call,
            serde_json::to_vec(&serde_json::json!(["a.txt"])).unwrap(),
        );
        assert_eq!(
            registry
                .package_for_world(call.world())
                .unwrap()
                .decode_reply(&call, reply)
                .unwrap(),
            serde_json::json!(["a.txt"])
        );
    }

    #[test]
    fn mount_and_reserved_name_collisions_fail_at_composition() {
        let duplicate = WorldClientRegistry::new()
            .with_package(package("com.example.files", "files"))
            .unwrap()
            .with_package(package("com.example.other-files", "files"));
        assert!(duplicate.is_err());

        let registry = WorldClientRegistry::new()
            .with_package(package("com.example.files", "files"))
            .unwrap();
        assert!(registry.validate_reserved(["files_list"]).is_err());
        assert!(registry.validate_reserved(["files"]).is_ok());
    }

    #[test]
    fn a_reserved_collision_is_scoped_to_the_package_being_mounted() {
        let files = package("com.example.files", "files");
        let notes = package("com.example.notes", "notes");
        assert!(
            files.validate_reserved(["notes_list"]).is_ok(),
            "a collision on an unpinned World emptied the session"
        );
        assert!(notes.validate_reserved(["notes_list"]).is_err());
        assert!(files.validate_reserved(["files_list"]).is_err());
    }

    #[test]
    fn an_unset_pin_takes_the_sole_world_and_refuses_when_there_are_two() {
        let one = WorldClientRegistry::new()
            .with_package(package("com.example.files", "files"))
            .unwrap();
        assert_eq!(one.pin(None).unwrap().mount(), "files");
        assert_eq!(one.pin(Some("files")).unwrap().mount(), "files");
        assert!(one.pin(Some("notes")).is_err());

        let two = WorldClientRegistry::new()
            .with_package(package("com.example.files", "files"))
            .unwrap()
            .with_package(package("com.example.notes", "notes"))
            .unwrap();
        let refused = match two.pin(None) {
            Ok(_) => panic!("two Worlds cannot default"),
            Err(error) => error,
        };
        assert!(
            refused
                .diagnostic()
                .is_some_and(|text| text.contains("LAIT_WORLD")),
            "{refused:?}"
        );
        assert_eq!(two.pin(Some("notes")).unwrap().mount(), "notes");
    }

    #[test]
    fn coverage_requires_every_command_to_be_a_tool_or_an_omission() {
        let gaps = agent_surface_coverage(["list", "geometry", "edit"], ["list"], &["geometry"]);
        assert_eq!(gaps.missing, vec!["edit".to_string()]);
        assert!(gaps.stale_without.is_empty());
        assert!(gaps.now_tooled.is_empty());
        assert!(gaps.check().is_err());

        let clean = agent_surface_coverage(["list", "geometry"], ["list"], &["geometry"]);
        assert!(clean.check().is_ok());
    }

    /// Every kind classifies itself, so a head's own policy never has to
    /// choose between guessing and refusing everything it cannot see into.
    #[test]
    fn every_invocation_declares_an_access_class() {
        let world =
            ClientInvocation::world(files_call(Value::Null).unwrap(), ClientAccess::Query, None);
        assert_eq!(world.access(), ClientAccess::Query);

        let local = ClientInvocation::local(
            WorldId::parse("com.example.files").unwrap(),
            "files.mark_read",
            Value::Null,
            ClientAccess::Command,
            None,
        );
        assert_eq!(local.access(), ClientAccess::Command);
    }

    fn block_on<T>(fut: impl Future<Output = T>) -> T {
        let mut fut = std::pin::pin!(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("test host must not pend"),
        }
    }

    struct FakeHost {
        payload: Value,
        identity: PresentationResolution,
        identity_calls: std::sync::Mutex<usize>,
    }

    impl ClientHost for FakeHost {
        fn local_root(&self) -> &Path {
            Path::new(".")
        }

        fn call_world<'a>(&'a self, call: Call) -> ClientFuture<'a, Reply> {
            let payload = self.payload.clone();
            Box::pin(async move {
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|error| Failure::new(error.to_string()))?;
                Ok(Reply::ok(&call, bytes))
            })
        }

        fn call_control<'a>(&'a self, _request: HostControlRequest) -> ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_work<'a>(
            &'a self,
            _request: runtime::exec::WorkRequest,
        ) -> ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_content<'a>(&'a self, _request: HostContentRequest) -> ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_identity<'a>(
            &'a self,
            _handles: Vec<PresentationHandle>,
        ) -> ClientFuture<'a, PresentationResolution> {
            *self.identity_calls.lock().expect("calls") += 1;
            let identity = self.identity.clone();
            Box::pin(async move { Ok(identity) })
        }
    }

    fn decorate_actor<'a>(
        host: &'a dyn ClientHost,
        _call: &'a Call,
        mut value: Value,
    ) -> ClientFuture<'a, Value> {
        Box::pin(async move {
            let resolution = host
                .call_identity(vec![PresentationHandle::actor("act_one")])
                .await?;
            if resolution.is_unavailable() {
                return Ok(value);
            }
            if let Some(name) = resolution.name_for_actor("act_one") {
                value["authored_name"] = Value::String(name.to_owned());
            }
            Ok(value)
        })
    }

    #[test]
    fn a_package_without_a_decorator_keeps_decoded_json() {
        let host = FakeHost {
            payload: serde_json::json!({"actor": "act_one"}),
            identity: PresentationResolution::unavailable(),
            identity_calls: std::sync::Mutex::new(0),
        };
        let package = package("com.example.files", "files");
        let invocation = files_invocation(Value::Null).unwrap();
        let value = block_on(package.execute(&host, invocation)).unwrap();
        assert_eq!(value, serde_json::json!({"actor": "act_one"}));
        assert_eq!(*host.identity_calls.lock().expect("calls"), 0);
    }

    #[test]
    fn unavailable_resolution_does_not_write_empty_names() {
        let host = FakeHost {
            payload: serde_json::json!({"actor": "act_one", "actor_nick": ""}),
            identity: PresentationResolution::unavailable(),
            identity_calls: std::sync::Mutex::new(0),
        };
        let package = package("com.example.files", "files").with_decorator(decorate_actor);
        let invocation = files_invocation(Value::Null).unwrap();
        let value = block_on(package.execute(&host, invocation)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"actor": "act_one", "actor_nick": ""})
        );
        assert!(value.get("authored_name").is_none());
        assert_eq!(*host.identity_calls.lock().expect("calls"), 1);
    }

    #[test]
    fn a_live_hit_adds_only_a_presentation_field() {
        let host = FakeHost {
            payload: serde_json::json!({"actor": "act_one"}),
            identity: PresentationResolution {
                labels: vec![PresentationLabel {
                    handle: PresentationHandle::actor("act_one"),
                    name: Some("Ada".into()),
                }],
                coverage: None,
            },
            identity_calls: std::sync::Mutex::new(0),
        };
        let package = package("com.example.files", "files").with_decorator(decorate_actor);
        let invocation = files_invocation(Value::Null).unwrap();
        let value = block_on(package.execute(&host, invocation)).unwrap();
        assert_eq!(value["actor"], "act_one");
        assert_eq!(value["authored_name"], "Ada");
        assert_eq!(*host.identity_calls.lock().expect("calls"), 1);
    }
}
