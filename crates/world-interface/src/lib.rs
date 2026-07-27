//! Client-facing application interfaces supplied by a World package.
//!
//! Runtime and [`world_bridge`] deliberately know nothing about presentation.
//! This crate is the outer compile-time seam: a product declares its CLI mount
//! and MCP tools, while the Lait shell supplies process lifecycle, Orbit
//! selection, transport, and output policy.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use clap::{ArgMatches, Command};
use replica::ids::WorldId;
use serde_json::Value;
use world_bridge::WorldCall;

/// A client-surface declaration or dispatch failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceError {
    message: String,
}

impl InterfaceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for InterfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InterfaceError {}

/// A parsed product CLI invocation.
///
/// `Local` is for product-owned client orchestration such as reading an
/// attachment before constructing its bounded World call or creating a branch
/// after a successful work-state call. The shell routes it back to the package;
/// it does not interpret the operation name.
#[derive(Debug, Clone)]
pub enum CliInvocation {
    World(WorldCall),
    Local { operation: String, input: Value },
}

pub type CliCommandFactory = fn() -> Command;
pub type CliParser = fn(&ArgMatches) -> Result<CliInvocation, InterfaceError>;

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

    pub fn parse(&self, matches: &ArgMatches) -> Result<CliInvocation, InterfaceError> {
        (self.parse)(matches)
    }
}

pub type McpSchemaFactory = fn() -> Value;
pub type McpCallFactory = fn(Value) -> Result<WorldCall, InterfaceError>;

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

    pub fn call(&self, input: Value) -> Result<WorldCall, InterfaceError> {
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
}

impl WorldClientPackage {
    pub fn new(
        world: WorldId,
        cli: CliMount,
        mcp_tools: Vec<McpTool>,
        mcp_instructions: &'static str,
    ) -> Result<Self, InterfaceError> {
        validate_name("CLI mount", cli.name())?;
        let mut local_tools = BTreeSet::new();
        for tool in &mcp_tools {
            validate_name("MCP tool", tool.name())?;
            if !local_tools.insert(tool.name()) {
                return Err(InterfaceError::new(format!(
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
        })
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

    pub fn with_package(mut self, package: WorldClientPackage) -> Result<Self, InterfaceError> {
        let world = package.world().as_str().to_string();
        let command_name = package.cli().command().get_name().to_string();
        if command_name != package.cli().name() {
            return Err(InterfaceError::new(format!(
                "World '{}' CLI factory produced command '{}' for mount '{}'",
                world,
                command_name,
                package.cli().name()
            )));
        }
        if self.packages.contains_key(&world) {
            return Err(InterfaceError::new(format!(
                "duplicate client package for World '{world}'"
            )));
        }
        if let Some(existing) = self.mounts.get(package.cli().name()) {
            return Err(InterfaceError::new(format!(
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
    ) -> Result<(), InterfaceError> {
        let reserved_cli: BTreeSet<_> = reserved_cli.into_iter().collect();
        let reserved_mcp: BTreeSet<_> = reserved_mcp.into_iter().collect();
        for package in self.packages.values() {
            if reserved_cli.contains(package.cli().name()) {
                return Err(InterfaceError::new(format!(
                    "World '{}' CLI mount '{}' collides with a shell command",
                    package.world(),
                    package.cli().name()
                )));
            }
            for mounted in mounted_tools(package) {
                if reserved_mcp.contains(mounted.public_name.as_str()) {
                    return Err(InterfaceError::new(format!(
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

fn validate_name(kind: &str, name: &str) -> Result<(), InterfaceError> {
    let valid = !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(InterfaceError::new(format!(
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

    fn files_call(_: Value) -> Result<WorldCall, InterfaceError> {
        WorldCall::new(
            WorldId::parse("com.example.files").unwrap(),
            "files.list",
            1,
            vec![],
        )
        .map_err(|error| InterfaceError::new(error.to_string()))
    }

    fn files_command() -> Command {
        Command::new("files").subcommand(Command::new("list"))
    }

    fn files_parse(_: &ArgMatches) -> Result<CliInvocation, InterfaceError> {
        Ok(CliInvocation::World(files_call(Value::Null)?))
    }

    fn package(world: &str, mount: &'static str) -> WorldClientPackage {
        WorldClientPackage::new(
            WorldId::parse(world).unwrap(),
            CliMount::new(mount, files_command, files_parse),
            vec![McpTool::new(
                "list",
                "List files.",
                empty_schema,
                files_call,
            )],
            "Work with files.",
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
