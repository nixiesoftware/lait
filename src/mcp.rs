//! MCP server over stdio composing shell and one pinned World's tools.
//!
//! The root owns identity, Orbit scope, transport, and Mechanics tools. The
//! session speaks one World (`$LAIT_WORLD`, or the sole World this build
//! hosts). That package designs the namespaced schemas, omissions, and
//! teaching text; this adapter mounts them. It does not generate tools from
//! the wire protocol, and it does not compose a second World onto the same
//! `tools/list`.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rmcp::{
    handler::server::{
        router::tool::{ToolRoute, ToolRouter},
        wrapper::Parameters,
    },
    model::{
        CacheScope, CallToolResult, ContentBlock, Implementation, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ResultType, ServerCapabilities, ServerInfo, Tool,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::Deserialize;

use crate::{
    control::{Request, Response},
    daemon::ClientScope,
    host_client::{client_as_scoped, scope_for_home},
};

/// The minimum complete shell operations an agent must be able to drive
/// alongside a World. World-owned verbs live on the package that designed
/// them — this list must not grow an `issues_*` (or any other product) name.
pub const REQUIRED_SHELL_COMMANDS: &[&str] = &[
    "member_add",
    "member_remove",
    "agent_add",
    "key_rotate",
    "members",
    "member_log",
];

fn pin_failure(error: world_interface::Failure) -> anyhow::Error {
    match error.diagnostic() {
        Some(text) => anyhow::anyhow!("{text}"),
        None => anyhow::anyhow!("{error}"),
    }
}

/// Shell/Mechanics tool names, asked of the macro-generated router so a
/// `#[tool]` cannot land without appearing on the declared surface.
pub fn shell_tool_names() -> Vec<String> {
    LaitMcp::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect()
}

/// The declared MCP surface for a pin: shell names plus that World's public tools.
pub fn declared_tool_names(
    registry: &world_interface::WorldClientRegistry,
    world: Option<&str>,
) -> Result<Vec<String>> {
    let package = registry.pin(world).map_err(pin_failure)?;
    let mut names = shell_tool_names();
    names.extend(
        package
            .mcp_tools()
            .iter()
            .map(|tool| format!("{}_{}", package.mount(), tool.name())),
    );
    Ok(names)
}

// ---- tool argument schemas ----

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MemberAddArgs {
    /// A who-ref: `@me`, a local alias, a key id-prefix, or a 64-hex ed25519 key.
    pub who: String,
    /// Grant the admin role.
    #[serde(default)]
    pub admin: bool,
    /// Optional local petname to attach to the resolved key (never synced).
    #[serde(default)]
    pub alias: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AgentAddArgs {
    /// The agent's ed25519 public key (64-hex) — the keypair the agent signs with.
    pub key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MemberRemoveArgs {
    pub who: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JoinArgs {
    /// A base32 space ticket from `invite_ticket`.
    pub ticket: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitArgs {
    /// Heads from the last `whoami` or `wait`. Same comparison as
    /// Exec Watch: matching heads mean nothing changed.
    #[serde(default)]
    pub heads: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindArgs {
    /// A Runtime `find::Query`: a typed operator DAG over the pinned World's
    /// declared schema. Publication may be omitted for the current read image.
    #[schemars(with = "serde_json::Value")]
    pub query: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConnectArgs {
    /// A base32 space ticket from a coworker's `invite_ticket`.
    pub ticket: String,
}

#[derive(Clone)]
pub struct LaitMcp {
    home: PathBuf,
    /// Capability scope fixed when this MCP server is constructed. It does not
    /// widen merely because the user-level daemon knows other local Orbits.
    scope: ClientScope,
    /// The local agent identity this server acts as (from `$LAIT_AGENT`), so
    /// every tool call is signed and attributed to the *agent*, not the human
    /// whose home hosts the daemon (Architecture B). `None` = the primary
    /// identity, the pre-B behavior. The human sponsors the agent once
    /// (`Request::AgentAdd`, or the client's sponsorship ask); MCP attaches as it after.
    act_as: Option<String>,
    /// Which identity's daemon this server talks to, carried from the
    /// invocation rather than re-read from the environment per call.
    selection: crate::config::Selection,
    /// Shell tools merged with the pinned World's namespaced tools. The
    /// `tool_handler` attribute on the `ServerHandler` impl must name this field
    /// explicitly, because its default is the macro-generated
    /// `Self::tool_router()`, which knows only the shell half. Left to that
    /// default, the merge below still runs and the field is simply never read:
    /// the World's tools are built, then dropped at serve time.
    tool_router: ToolRouter<LaitMcp>,
    /// Mount of the one World this session speaks. Teaching text comes from
    /// that package; the shell does not name another product's verbs.
    world_mount: String,
    world_instructions: String,
    /// Reverse-domain id of the pin. `world_upgrade` names this World, and
    /// an invocation for any other is refused rather than routed.
    world_id: String,
    registry: std::sync::Arc<world_interface::WorldClientRegistry>,
}

#[tool_router]
impl LaitMcp {
    pub fn new(home: PathBuf, selection: crate::config::Selection) -> Result<Self> {
        Self::from_pins(
            home,
            selection,
            std::env::var("LAIT_AGENT").ok().filter(|s| !s.is_empty()),
            std::env::var("LAIT_WORLD").ok().filter(|s| !s.is_empty()),
        )
    }

    /// Construct with explicit pins. Tests use this so a process-wide
    /// `$LAIT_WORLD` cannot silently change the default surface.
    pub fn from_pins(
        home: PathBuf,
        selection: crate::config::Selection,
        act_as: Option<String>,
        world: Option<String>,
    ) -> Result<Self> {
        let identity = selection.identity_dir()?;
        let registry = std::sync::Arc::new(
            crate::world::installed::load(&crate::serve::head::installations_root(&identity))?
                .clients,
        );
        Self::from_registry(home, selection, act_as, world, registry)
    }

    fn from_registry(
        home: PathBuf,
        selection: crate::config::Selection,
        act_as: Option<String>,
        world: Option<String>,
        registry: std::sync::Arc<world_interface::WorldClientRegistry>,
    ) -> Result<Self> {
        let scope = scope_for_home(&home);
        let package = registry.pin(world.as_deref()).map_err(pin_failure)?;
        let shell = Self::tool_router();
        // Only the pin is composed, so only the pin is checked. A collision
        // on an unpinned World must not empty this session, and a collision
        // on the pin must refuse construct — silent empty is how 56 tools
        // vanished from the wire once already.
        package
            .validate_reserved(shell.list_all().iter().map(|tool| tool.name.as_ref()))
            .map_err(pin_failure)?;
        let mut tool_router = shell;
        tool_router.merge(Self::world_tool_router(package));
        let world_mount = package.mount().to_owned();
        let world_instructions = package.mcp_instructions().to_owned();
        let world_id = package.world().as_str().to_owned();
        Ok(Self {
            home,
            scope,
            act_as,
            selection,
            tool_router,
            world_mount,
            world_instructions,
            world_id,
            registry,
        })
    }

    fn world_tool_router(package: &world_interface::WorldClientPackage) -> ToolRouter<Self> {
        let mut router = ToolRouter::new();
        for tool in package.mcp_tools() {
            let Some(schema) = tool.schema().as_object().cloned() else {
                continue;
            };
            let public_name = format!("{}_{}", package.mount(), tool.name());
            let tool = tool.clone();
            let route = ToolRoute::new_dyn(
                Tool::new(public_name, tool.description(), schema),
                move |context: rmcp::handler::server::tool::ToolCallContext<'_, Self>| {
                    let tool = tool.clone();
                    Box::pin(async move {
                        let input =
                            serde_json::Value::Object(context.arguments.unwrap_or_default());
                        let invocation = match tool.call(input) {
                            Ok(invocation) => invocation,
                            Err(error) => {
                                return Ok(Self::tool_error(
                                    error
                                        .diagnostic()
                                        .unwrap_or("invalid tool arguments")
                                        .to_owned(),
                                )
                                .into());
                            }
                        };
                        context
                            .service
                            .run_invocation(invocation)
                            .await
                            .map(Into::into)
                    })
                },
            );
            router.add_route(route);
        }
        router
    }

    /// Drive the daemon and return its `Response` as JSON text (the same
    /// versioned DTO the local app's HTTP surface emits).
    ///
    /// Failures are mapped to **typed, actionable** MCP errors rather than an
    /// opaque `internal_error(blob)`: an authorization failure (`Denied`) — the
    /// first wall a freshly-sponsored agent hits — becomes an `invalid_request`
    /// carrying the daemon's actionable message (what standing is missing and
    /// how to get it), a `NotFound` becomes an `invalid_request`, and only a
    /// genuine internal/transport failure stays `internal_error`. The daemon's
    /// message already names the next step; MCP just preserves the typing so the
    /// agent isn't told "internal error" for something it can act on.
    async fn run(&self, req: Request) -> Result<CallToolResult, McpError> {
        let response = client_as_scoped(
            &self.home,
            req,
            &self.scope,
            self.act_as.as_deref(),
            &self.selection,
        )
        .await;
        Self::tool_result(response)
    }

    async fn run_invocation(
        &self,
        invocation: world_interface::ClientInvocation,
    ) -> Result<CallToolResult, McpError> {
        if invocation.world_id().as_str() != self.world_id {
            return Err(McpError::invalid_request(
                format!(
                    "this session is pinned to World '{}'; '{}' is not served here",
                    self.world_id,
                    invocation.world_id()
                ),
                None,
            ));
        }
        let package = self
            .registry
            .package_for_world(invocation.world_id())
            .cloned()
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("no client package for World '{}'", invocation.world_id()),
                    None,
                )
            })?;
        // `for_home` derives the same pinned scope this server was constructed
        // with; a second spelling of it here is a place for the two to drift.
        let host = crate::host_client::PackageClientHost::for_home(
            &self.home,
            self.act_as.clone(),
            self.selection.clone(),
        )
        .map_err(|error| McpError::invalid_request(format!("{error:#}"), None))?;
        let value = match package.execute(&host, invocation).await {
            Ok(value) => value,
            Err(error) => {
                return Ok(Self::tool_error(
                    error
                        .diagnostic()
                        .unwrap_or("invalid client operation")
                        .to_owned(),
                ));
            }
        };
        // A product answer that reports a failure arrived intact — only its own
        // package can say so. Caller-actionable refusals stay in the tool
        // result so the model sees the message (SEP-1303). JSON-RPC errors are
        // reserved for transport and unknown methods.
        if package.classify_failure(&value).is_some() {
            return Ok(CallToolResult::structured_error(value));
        }
        Ok(CallToolResult::structured(value))
    }

    fn tool_error(message: String) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(message)])
    }

    fn tool_result(response: anyhow::Result<Response>) -> Result<CallToolResult, McpError> {
        match response {
            Ok(Response::Error { message, .. }) => Ok(Self::tool_error(message)),
            Ok(resp) => {
                let value = serde_json::to_value(&resp)
                    .unwrap_or_else(|_| serde_json::json!({"kind":"ok"}));
                Ok(CallToolResult::structured(value))
            }
            Err(e) => Err(McpError::internal_error(format!("{e:#}"), None)),
        }
    }

    // ---- membership and authorization ----

    #[tool(description = "Add a space member (admin-only); seals them the space key.")]
    async fn member_add(
        &self,
        Parameters(a): Parameters<MemberAddArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(Request::MemberAdd {
            who: a.who,
            admin: a.admin,
            as_name: a.alias,
        })
        .await
    }

    #[tool(
        description = "Remove a space member (admin-only) and rotate the key (lazy revocation)."
    )]
    async fn member_remove(
        &self,
        Parameters(a): Parameters<MemberRemoveArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(Request::MemberRemove { who: a.who }).await
    }

    #[tool(
        description = "Sponsor an agent keypair (any human member). The agent gets content \
                       authority — the same standing an ordinary member writes with, including \
                       closing and deleting issues — and never membership authority: it cannot \
                       admit, remove, or re-role anyone. Its standing dies with the sponsor."
    )]
    async fn agent_add(
        &self,
        Parameters(a): Parameters<AgentAddArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(Request::AgentAdd { key: a.key }).await
    }

    #[tool(description = "Rotate the space key (admin-only).")]
    async fn key_rotate(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::KeyRotate).await
    }

    #[tool(description = "List space members and their roles (from the signed ACL).")]
    async fn members(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Members).await
    }

    #[tool(
        description = "The membership audit log: the signed ACL DAG replayed in causal order, \
                       with each op's authorization verdict (cryptographic provenance)."
    )]
    async fn member_log(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::MemberLog).await
    }

    // ---- transport / presence ----

    #[tool(description = "Show node + space status: id, nick, space, item/scope counts.")]
    async fn status(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Status).await
    }

    #[tool(
        description = "Guided-join verifier: an ordered readout of the onboarding gates \
                       (space, daemon, membership, peer, sync) with the one blocker \
                       named in `blocked_on`. Use it to explain why the board is empty or \
                       a join hasn't completed."
    )]
    async fn doctor(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Diagnose {
            expected_space: None,
        })
        .await
    }

    #[tool(
        description = "Make this build's World implementation the space's active one \
                       (admin only). A node whose build is NEWER already does this by \
                       itself at startup; this is the deliberate form, and the only way \
                       to move the space BACK onto an older build. Check `doctor`'s \
                       `implementation` gate first — it names both versions."
    )]
    async fn world_upgrade(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::WorldActivate {
            world: self.world_id.clone(),
        })
        .await
    }

    #[tool(description = "Get this node's endpoint id — the handle a coworker uses to reach us.")]
    async fn my_id(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Id).await
    }

    #[tool(
        description = "Produce a base32 space ticket to share so a coworker can join. The ticket carries a signed, single-use pass so they are auto-admitted on join (no separate approve step)."
    )]
    async fn invite_ticket(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Invite {
            world: Some(self.world_id.clone()),
            role: None,
            reusable: false,
            ttl_hours: None,
        })
        .await
    }

    #[tool(
        description = "Connect to the bound space via a ticket for it and broadcast a request \
                       to be added. MCP runs against an already-bound store: a ticket for a \
                       different space errors (enter it from the local app first)."
    )]
    async fn join_room(
        &self,
        Parameters(a): Parameters<JoinArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(Request::Join { ticket: a.ticket }).await
    }

    #[tool(description = "One-step onboarding: connect to a space from a ticket (joins + live).")]
    async fn connect(
        &self,
        Parameters(a): Parameters<ConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(Request::Connect { ticket: a.ticket }).await
    }

    #[tool(description = "List known peers and whether they are online.")]
    async fn who(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Who).await
    }

    #[tool(
        description = "Who am I here? Your actor id, did:key, role, capabilities, sponsor, \
                       space, and whether your view is complete — in one shot. Call this \
                       first: it tells you if you are a member (attach) or need sponsoring, \
                       and whether it is safe to author (partial_view must be false). If \
                       member is false and sponsorship_asked is true, call wait with \
                       wait_heads — that is Exec Watch, not a whoami poll."
    )]
    async fn whoami(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Whoami).await
    }

    #[tool(
        description = "Watch the host-plane sponsorship wait. Same shape as Exec Watch: \
                       pass heads from whoami.wait_heads or the last wait. \
                       Unchanged means still waiting; granted means you are in — proceed \
                       as yourself. Not a live stream; call again with the returned heads."
    )]
    async fn wait(&self, Parameters(a): Parameters<WaitArgs>) -> Result<CallToolResult, McpError> {
        self.run(Request::SponsorWatch { heads: a.heads }).await
    }

    #[tool(
        description = "Converge now and report whether your view is complete. Call before \
                       acting on a 'close what's done' style request: it names any missing \
                       epoch key loudly, and the daemon refuses to let you author against a \
                       known-partial view."
    )]
    async fn sync(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Sync).await
    }

    #[tool(
        description = "Run one typed bounded query over the pinned World's immutable shared \
                       corpus. The answer is stamped with manifest, implementation, extractor, \
                       materialization, actor, device and authority coordinates. Use exact/term/\
                       id seeks for deep lookup; use a Body seek to resolve every item named by \
                       one live change in a single call."
    )]
    async fn find(&self, Parameters(a): Parameters<FindArgs>) -> Result<CallToolResult, McpError> {
        let query = serde_json::from_value(a.query).map_err(|error| {
            McpError::invalid_request(format!("invalid Runtime Find query: {error}"), None)
        })?;
        self.run(Request::Find {
            world: self.world_id.clone(),
            query,
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LaitMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
            .with_instructions(format!(
                "You attach to the '{}' World on a local-first peer-to-peer Space. \
                 You have your OWN identity — you do not rebuild or re-join per session; you \
                 attach. Start by calling whoami: it reports who you are (your actor + \
                 did:key), your role and capabilities, who sponsors you, and the space. If \
                 whoami shows you are not yet a member, sponsorship has been requested from \
                 the person on this machine (their local client). Call wait with \
                 wait_heads and keep calling it with the heads it returns — that is Exec \
                 Watch, not a whoami poll and not invite→connect. When wait returns \
                 granted, you hold write access and act as yourself. \
                 {} Before acting on a 'close what's done' style request, \
                 call sync — it converges and refuses to let you author against a known-partial \
                 view. Every tool returns the same versioned JSON DTO the local app reads; \
                 a denied action tells you exactly what standing you lack and how to get it.",
                self.world_mount, self.world_instructions
            ))
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[
            ProtocolVersion::V_2026_07_28,
            ProtocolVersion::V_2025_11_25,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2024_11_05,
        ])
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let modern = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.tool_router.list_all(),
            meta: None,
            next_cursor: None,
            ttl_ms: modern.then_some(60_000),
            cache_scope: modern.then_some(CacheScope::Private),
        })
    }
}

/// Run the MCP server over stdio until the client disconnects.
pub async fn run_mcp(home: &Path, selection: crate::config::Selection) -> Result<()> {
    let service = LaitMcp::new(home.to_path_buf(), selection)?
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use crate::control::OrbitAddress;
    use mechanics::ids::SpaceId;

    fn mcp(home: &str, world: &str) -> Result<LaitMcp> {
        LaitMcp::from_registry(
            PathBuf::from(home),
            crate::config::Selection::default(),
            None,
            Some(world.into()),
            std::sync::Arc::new(crate::world::client_packages()),
        )
    }

    #[test]
    fn an_mcp_server_is_pinned_to_the_home_it_was_constructed_for() {
        let home = PathBuf::from("/tmp/lait-mcp-a");
        let sibling = PathBuf::from("/tmp/lait-mcp-b");
        let space = SpaceId::from_digest([9; 16]);
        let mcp = LaitMcp::from_registry(
            home.clone(),
            crate::config::Selection::default(),
            None,
            Some("issues".into()),
            std::sync::Arc::new(crate::world::client_packages()),
        )
        .expect("issues is hosted");
        let own = OrbitAddress::for_store(&home, space.clone());
        let other = OrbitAddress::for_store(&sibling, space);

        assert!(mcp.scope.authorize(&own).is_ok());
        assert!(mcp.scope.authorize(&other).is_err());
    }

    #[test]
    fn the_live_router_composes_shell_and_namespaced_world_tools() {
        let mcp = mcp("/tmp/lait-mcp-tools", "issues").expect("issues is hosted");
        let names: Vec<_> = mcp
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        let declared = declared_tool_names(&crate::world::client_packages(), Some("issues"))
            .expect("issues is hosted");
        for expected in &declared {
            assert!(names.iter().any(|name| name == expected), "{expected}");
        }
        assert!(!names.iter().any(|name| name == "issue_new"));
        assert_eq!(names.len(), declared.len());
    }

    #[test]
    fn world_upgrade_names_the_pinned_world() {
        let mcp = mcp("/tmp/lait-mcp-upgrade", "issues").expect("issues is hosted");
        assert_eq!(mcp.world_id, crate::world::contract::world_id().as_str());
        assert_eq!(mcp.world_mount, "issues");
    }

    #[test]
    fn an_unknown_world_pin_is_refused_rather_than_served_empty() {
        // "signage" stopped being the example the day it became hosted; the
        // mount here must stay one no build carries.
        let error = match mcp("/tmp/lait-mcp-world", "atlas") {
            Ok(_) => panic!("an unhosted World was mounted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("LAIT_WORLD=atlas"), "{error:#}");
    }

    #[test]
    fn the_issues_mount_is_a_legal_explicit_pin() {
        let mcp = mcp("/tmp/lait-mcp-issues", "issues").expect("issues is hosted");
        assert!(mcp
            .tool_router
            .list_all()
            .iter()
            .any(|tool| tool.name == "issues_list"));
    }
}
