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
//! Runtime's World-call boundary deliberately knows nothing about presentation.
//! This crate is the outer compile-time seam: a product declares its CLI mount
//! and MCP tools, while the Lait shell supplies process lifecycle, Orbit
//! selection, transport, and output policy.

pub mod destination;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use clap::{ArgMatches, Command};
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

/// The complete externally visible effect of one client invocation.
///
/// This classifies the whole package-owned operation, including caller-local
/// effects such as advancing a watermark or writing an attachment. It is not a
/// substitute for the daemon's independent [`runtime::world::call::Access`]
/// classification of an opaque World call.
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

pub type CliCommandFactory = fn() -> Command;
pub type CliParser = fn(&ArgMatches) -> Result<ClientInvocation, Failure>;

/// One root-level CLI namespace mounted by a World application.
#[derive(Clone)]
pub struct CliMount {
    name: &'static str,
    command: CliCommandFactory,
    parse: CliParser,
}

impl CliMount {
    pub fn new(name: &'static str, command: CliCommandFactory, parse: CliParser) -> Self {
        Self {
            name,
            command,
            parse,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn command(&self) -> Command {
        (self.command)()
    }

    pub fn parse(&self, matches: &ArgMatches) -> Result<ClientInvocation, Failure> {
        (self.parse)(matches)
    }
}

pub type McpSchemaFactory = fn() -> Value;
pub type McpCallFactory = fn(Value) -> Result<ClientInvocation, Failure>;
pub type ReplyDecoder = fn(&Call, Reply) -> Result<Value, Failure>;
pub type ReplyPresenter = fn(Value, PresentationOptions) -> Result<Presentation, Failure>;
pub type WebParser = fn(Value) -> Result<ClientInvocation, Failure>;

/// Output policy supplied by the navigation shell to a product presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationOptions {
    pub json: bool,
    pub color: bool,
}

/// How a product-classified failure should surface to an agent client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationFailure {
    InvalidRequest,
    Internal,
}

/// A complete product-owned rendering result.
///
/// The shell writes these strings to their named streams and uses the exit code;
/// it never needs to decode or match the product response itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presentation {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub failure: Option<PresentationFailure>,
    pub failure_message: Option<String>,
}

/// A package execution result shared by CLI, MCP, and web adapters.
///
/// `value` is the lossless machine result. `presentation` is optional
/// product-owned terminal/error policy; native clients render it while web
/// clients return `value` directly.
#[derive(Debug, Clone)]
pub struct ClientOutput {
    pub value: Value,
    pub presentation: Option<Presentation>,
}

impl ClientOutput {
    pub fn new(value: Value, presentation: Option<Presentation>) -> Self {
        Self {
            value,
            presentation,
        }
    }
}

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

pub type LocalInvocationHandler = for<'a> fn(
    &'a dyn ClientHost,
    LocalInvocation,
    PresentationOptions,
) -> ClientFuture<'a, ClientOutput>;

/// Resolve the confirmation prompt for one invocation, with a host available.
///
/// A parse-time question can only name the *selector* the user typed, which for
/// a ref inferred from the working tree is unanswerable. This hook lets the
/// package read enough to name the thing itself before anyone is asked.
pub type ConfirmationResolver =
    for<'a> fn(&'a dyn ClientHost, &'a ClientInvocation) -> ClientFuture<'a, Option<String>>;

/// One product-local MCP tool. The registry prefixes `name` with the CLI mount,
/// so independently developed Worlds cannot both publish a global `list`.
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
    cli: CliMount,
    mcp_tools: Vec<McpTool>,
    mcp_instructions: &'static str,
    decode_reply: ReplyDecoder,
    present_reply: Option<ReplyPresenter>,
    local_handler: Option<LocalInvocationHandler>,
    web_parser: Option<WebParser>,
    confirmation: Option<ConfirmationResolver>,
}

impl WorldClientPackage {
    pub fn new(
        world: WorldId,
        cli: CliMount,
        mcp_tools: Vec<McpTool>,
        mcp_instructions: &'static str,
        decode_reply: ReplyDecoder,
    ) -> Result<Self, Failure> {
        validate_name("CLI mount", cli.name())?;
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
            cli,
            mcp_tools,
            mcp_instructions,
            decode_reply,
            present_reply: None,
            local_handler: None,
            web_parser: None,
            confirmation: None,
        })
    }

    pub fn with_presenter(mut self, presenter: ReplyPresenter) -> Self {
        self.present_reply = Some(presenter);
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

    pub fn cli(&self) -> &CliMount {
        &self.cli
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

    pub fn present_reply(
        &self,
        value: Value,
        options: PresentationOptions,
    ) -> Result<Option<Presentation>, Failure> {
        self.present_reply
            .map(|presenter| presenter(value, options))
            .transpose()
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

    /// Execute one invocation through its owning package.
    pub fn execute<'a>(
        &'a self,
        host: &'a dyn ClientHost,
        invocation: ClientInvocation,
        options: PresentationOptions,
    ) -> ClientFuture<'a, ClientOutput> {
        Box::pin(async move {
            self.validate_invocation(&invocation)?;
            match invocation.into_kind() {
                ClientInvocationKind::World(call) => {
                    let reply = host.call_world(call.clone()).await?;
                    let value = self.decode_reply(&call, reply)?;
                    let presentation = self.present_reply(value.clone(), options)?;
                    Ok(ClientOutput::new(value, presentation))
                }
                ClientInvocationKind::Local(local) => {
                    let handler = self.local_handler.ok_or_else(|| {
                        Failure::new(format!(
                            "World '{}' does not expose local client operations",
                            self.world
                        ))
                    })?;
                    handler(host, local, options).await
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
        let command_name = package.cli().command().get_name().to_string();
        if command_name != package.cli().name() {
            return Err(Failure::new(format!(
                "World '{}' CLI factory produced command '{}' for mount '{}'",
                world,
                command_name,
                package.cli().name()
            )));
        }
        if self.packages.contains_key(&world) {
            return Err(Failure::new(format!(
                "duplicate client package for World '{world}'"
            )));
        }
        if let Some(existing) = self.mounts.get(package.cli().name()) {
            return Err(Failure::new(format!(
                "CLI mount '{}' is claimed by Worlds '{}' and '{}'",
                package.cli().name(),
                existing,
                world
            )));
        }
        self.mounts.insert(package.cli().name(), world.clone());
        self.packages.insert(world, package);
        Ok(self)
    }

    pub fn validate_reserved<'a>(
        &self,
        reserved_cli: impl IntoIterator<Item = &'a str>,
        reserved_mcp: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), Failure> {
        let reserved_cli: BTreeSet<_> = reserved_cli.into_iter().collect();
        let reserved_mcp: BTreeSet<_> = reserved_mcp.into_iter().collect();
        for package in self.packages.values() {
            if reserved_cli.contains(package.cli().name()) {
                return Err(Failure::new(format!(
                    "World '{}' CLI mount '{}' collides with a shell command",
                    package.world(),
                    package.cli().name()
                )));
            }
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
        public_name: format!("{}_{}", package.cli().name(), tool.name()),
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

    fn files_command() -> Command {
        Command::new("files").subcommand(Command::new("list"))
    }

    fn files_parse(_: &ArgMatches) -> Result<ClientInvocation, Failure> {
        Ok(ClientInvocation::world(
            files_call(Value::Null)?,
            ClientAccess::Query,
            None,
        ))
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

    fn notes_command() -> Command {
        Command::new("notes").subcommand(Command::new("list"))
    }

    fn notes_parse(_: &ArgMatches) -> Result<ClientInvocation, Failure> {
        Ok(ClientInvocation::world(
            notes_call(Value::Null)?,
            ClientAccess::Query,
            None,
        ))
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
        let (cli, call) = if mount == "notes" {
            (
                CliMount::new(mount, notes_command, notes_parse),
                notes_invocation as McpCallFactory,
            )
        } else {
            (
                CliMount::new(mount, files_command, files_parse),
                files_invocation as McpCallFactory,
            )
        };
        WorldClientPackage::new(
            WorldId::parse(world).unwrap(),
            cli,
            vec![McpTool::new("list", "List objects.", empty_schema, call)],
            "Work with files.",
            decode_json_reply,
        )
        .unwrap()
    }

    #[test]
    fn a_second_world_mounts_without_shell_specific_code() {
        let registry = WorldClientRegistry::new()
            .with_package(package("com.example.files", "files"))
            .unwrap()
            .with_package(package("com.example.notes", "notes"))
            .unwrap();
        let mounts: Vec<_> = registry
            .packages()
            .map(|package| package.cli().name())
            .collect();
        assert_eq!(mounts, vec!["files", "notes"]);
        let tools: Vec<_> = registry.mcp_tools().map(|tool| tool.public_name).collect();
        assert_eq!(tools, vec!["files_list", "notes_list"]);

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
        assert!(registry.validate_reserved(["files"], []).is_err());
        assert!(registry.validate_reserved([], ["files_list"]).is_err());
    }
}
