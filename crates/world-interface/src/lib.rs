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
//! is displayed, and neither does this crate. It is the outer compile-time
//! seam: a product declares its mount name, its MCP tools, and how to decode a
//! reply into a value; the application composing lait supplies process
//! lifecycle, Orbit selection, transport, and every byte a human eventually
//! reads.
//!
//! Nothing here renders. A head that wants a table, a terminal line, or an HTML
//! page builds it from the [`serde_json::Value`] an invocation answers with —
//! which is why executing one costs a decode and nothing else.

pub mod destination;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use replica::body::WorldId;
use runtime::world::call::{Call, Reply};
use serde_json::Value;

/// A typed client-surface failure.
///
/// Concrete adapter diagnostics are logged at conversion and are deliberately
/// not retained in this public value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// A declaration, invocation, or returned value was invalid.
    Invalid,
    /// A valid operation was refused by the selected surface.
    Refusal,
    /// An accepted operation could not be completed.
    Operation,
    /// An established client operation ended before completion.
    Interruption,
}

impl Failure {
    pub fn new(message: impl fmt::Display) -> Self {
        tracing::warn!(diagnostic = %message, "World client adapter rejected an operation");
        Self::Invalid
    }

    pub const fn refusal() -> Self {
        Self::Refusal
    }

    pub const fn operation() -> Self {
        Self::Operation
    }

    pub const fn interruption() -> Self {
        Self::Interruption
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid client operation",
            Self::Refusal => "client operation refused",
            Self::Operation => "client operation failed",
            Self::Interruption => "client operation interrupted",
        })
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAccess {
    Query,
    Command,
}

/// One package-owned local operation.
#[derive(Debug, Clone)]
pub struct LocalInvocation {
    pub operation: String,
    pub input: Value,
}

/// The target selected by a parsed product invocation.
#[derive(Debug, Clone)]
pub enum ClientInvocationKind {
    World(Call),
    Local(LocalInvocation),
}

/// A parsed product invocation with package-owned policy metadata.
///
/// `Local` operations may compose World calls with working-tree, filesystem,
/// caller-local state, or generic Space-authority facilities. The shell
/// enforces the declared whole-operation access and confirmation policy, then
/// routes execution back through the package without interpreting its name.
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct HostAssignment {
    pub world: String,
    pub capability: String,
    pub resource: Vec<String>,
}

/// Boxed future used to keep the host interface dyn-compatible.
pub type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 'a>>;

/// Facilities supplied by a trusted native client host to a World package.
///
/// The package owns orchestration and product semantics. The implementation
/// owns Orbit selection, daemon transport, acting identity, and generic Space
/// authority. `local_root` is caller-local state, never replicated World state.
pub trait ClientHost: Send + Sync {
    fn local_root(&self) -> &Path;
    fn call_world<'a>(&'a self, call: Call) -> ClientFuture<'a, Reply>;
    fn call_control<'a>(&'a self, request: HostControlRequest) -> ClientFuture<'a, Value>;
    /// Move bytes on and off the content plane.
    ///
    /// Separate from [`Self::call_control`] because its answers are about a
    /// file rather than about Space authority, and because the shell streams
    /// here — the package never holds an attachment in memory, whatever its
    /// size.
    fn call_content<'a>(&'a self, request: HostContentRequest) -> ClientFuture<'a, Value>;
}

pub type LocalInvocationHandler =
    for<'a> fn(&'a dyn ClientHost, LocalInvocation) -> ClientFuture<'a, Value>;

/// Resolve the confirmation prompt for one invocation, with a host available.
///
/// A parse-time question can only name the *selector* the user typed, which for
/// a ref inferred from the working tree is unanswerable. This hook lets the
/// package read enough to name the thing itself before anyone is asked.
pub type ConfirmationResolver =
    for<'a> fn(&'a dyn ClientHost, &'a ClientInvocation) -> ClientFuture<'a, Option<String>>;

/// One product-local MCP tool. The registry prefixes `name` with the package's
/// mount, so independently developed Worlds cannot both publish a global `list`.
#[derive(Clone)]
pub struct McpTool {
    name: &'static str,
    description: &'static str,
    schema: McpSchemaFactory,
    call: McpCallFactory,
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
            schema,
            call,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn schema(&self) -> Value {
        (self.schema)()
    }

    pub fn call(&self, input: Value) -> Result<ClientInvocation, Failure> {
        (self.call)(input)
    }
}

/// The client interfaces shipped by one World application package.
#[derive(Clone)]
pub struct WorldClientPackage {
    world: WorldId,
    mount: &'static str,
    mcp_tools: Vec<McpTool>,
    mcp_instructions: &'static str,
    decode_reply: ReplyDecoder,
    classify_failure: Option<FailureClassifier>,
    local_handler: Option<LocalInvocationHandler>,
    web_parser: Option<WebParser>,
    confirmation: Option<ConfirmationResolver>,
    display: Display,
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
}

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
}

impl WorldClientPackage {
    /// Declare one package under `mount`, the single namespace key every head
    /// addresses it by.
    ///
    /// `mount` is not decoration. It prefixes every public MCP tool name and it
    /// is the `{world}` segment of the HTTP RPC route, so changing it renames
    /// every tool an agent has learned and breaks every URL a head has built.
    /// Treat it as published.
    pub fn new(
        world: WorldId,
        mount: &'static str,
        mcp_tools: Vec<McpTool>,
        mcp_instructions: &'static str,
        decode_reply: ReplyDecoder,
    ) -> Result<Self, Failure> {
        validate_name("mount", mount)?;
        let mut local_tools = BTreeSet::new();
        for tool in &mcp_tools {
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
            mount,
            mcp_tools,
            mcp_instructions,
            decode_reply,
            classify_failure: None,
            local_handler: None,
            web_parser: None,
            confirmation: None,
            display: Display::unstated(mount),
        })
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

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    /// The namespace key every head addresses this package by — the MCP tool
    /// prefix and the `{world}` route segment. See [`Self::new`].
    pub fn mount(&self) -> &'static str {
        self.mount
    }

    pub fn mcp_tools(&self) -> &[McpTool] {
        &self.mcp_tools
    }

    pub fn mcp_instructions(&self) -> &'static str {
        self.mcp_instructions
    }

    pub fn decode_reply(&self, call: &Call, reply: Reply) -> Result<Value, Failure> {
        (self.decode_reply)(call, reply)
    }

    /// Ask the package whether a delivered answer reports a failure, and of
    /// what class. See [`FailureClassifier`].
    pub fn classify_failure(&self, value: &Value) -> Option<(Failure, String)> {
        self.classify_failure.and_then(|classify| classify(value))
    }

    pub fn parse_web(&self, input: Value) -> Result<ClientInvocation, Failure> {
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
            let declared = invocation.confirmation_question().map(str::to_string);
            let Some(resolver) = self.confirmation else {
                return Ok(declared);
            };
            Ok(resolver(host, invocation).await.unwrap_or(declared))
        })
    }

    /// Execute one invocation through its owning package and answer with the
    /// decoded value — one decode per call, and nothing else.
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
            match invocation.into_kind() {
                ClientInvocationKind::World(call) => {
                    let reply = host.call_world(call.clone()).await?;
                    self.decode_reply(&call, reply)
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
            }
        })
    }

    fn validate_invocation(&self, invocation: &ClientInvocation) -> Result<(), Failure> {
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
    mounts: BTreeMap<&'static str, String>,
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
        self.mounts.insert(package.mount(), world.clone());
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
            for mounted in mounted_tools(package) {
                if reserved_mcp.contains(mounted.public_name.as_str()) {
                    return Err(Failure::new(format!(
                        "World '{}' MCP tool '{}' collides with a shell tool",
                        package.world(),
                        mounted.public_name
                    )));
                }
            }
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
            vec![McpTool::new("list", "List objects.", empty_schema, call)],
            "Work with files.",
            decode_json_reply,
        )
        .unwrap()
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

    /// Both kinds classify themselves, so a head's own policy never has to
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
}
