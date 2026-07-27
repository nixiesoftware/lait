//! MCP server over stdio composing shell and World-package tools.
//!
//! The root owns identity, Orbit scope, transport, and Mechanics tools. Each
//! World package contributes namespaced schemas and call factories to the same
//! RMCP router.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rmcp::{
    handler::server::{
        router::tool::{ToolRoute, ToolRouter},
        wrapper::Parameters,
    },
    model::{
        CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
        Tool,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::Deserialize;

use crate::{
    cli::{client_action_as_scoped, client_as_scoped, scope_for_home},
    client_action::ClientAction,
    control::{ErrorKind, Request, Response},
    daemon::ClientScope,
};

/// The replica command tags (`Request` serde `cmd` values) an agent must be able
/// to drive. `tests/mcp_parity.rs` asserts every one has a tool below, so adding
/// a `Request` without an MCP tool fails the interface-parity build gate.
pub const REQUIRED_TRACKER_COMMANDS: &[&str] = &[
    "issues_new",
    "issues_edit",
    "issues_move",
    "issues_start",
    "issues_done",
    "issues_stop",
    "issues_inbox",
    "issues_assign",
    "issues_label",
    "issues_comment",
    "issues_react",
    "issues_delete",
    "issues_restore",
    "issues_link",
    "issues_unlink",
    "issues_parent",
    "issues_graph",
    "issues_view",
    "issues_list",
    "issues_board",
    "issues_history",
    "issues_project_new",
    "issues_project_list",
    "issues_label_new",
    "issues_label_list",
    "issues_activity",
    "issues_role_list",
    "issues_role_show",
    "issues_role_create",
    "issues_role_edit",
    "issues_role_delete",
    "issues_role_resolve",
    "issues_access_list",
    "issues_access_grant",
    "issues_access_revoke",
    "issues_workflow_show",
    "issues_workflow_validate",
    "issues_workflow_set",
    // Shell/Mechanics tools needed alongside the product.
    "member_add",
    "member_remove",
    "agent_add",
    "key_rotate",
    "members",
    "member_log",
];

/// The set of MCP tool names this server exposes (kept beside the `#[tool]`
/// methods; the parity test cross-checks it covers `REQUIRED_TRACKER_COMMANDS`).
pub const MCP_TOOL_NAMES: &[&str] = &[
    // Issues application package (mounted as `issues_*`).
    "issues_new",
    "issues_start",
    "issues_done",
    "issues_stop",
    "issues_inbox",
    "issues_edit",
    "issues_move",
    "issues_assign",
    "issues_label",
    "issues_comment",
    "issues_react",
    "issues_delete",
    "issues_restore",
    "issues_link",
    "issues_unlink",
    "issues_parent",
    "issues_graph",
    "issues_view",
    "issues_list",
    "issues_board",
    "issues_history",
    "issues_project_new",
    "issues_project_list",
    "issues_label_new",
    "issues_label_list",
    "issues_activity",
    "issues_role_list",
    "issues_role_show",
    "issues_role_create",
    "issues_role_edit",
    "issues_role_delete",
    "issues_role_resolve",
    "issues_access_list",
    "issues_access_grant",
    "issues_access_revoke",
    "issues_workflow_show",
    "issues_workflow_validate",
    "issues_workflow_set",
    // Mechanics and shell.
    "member_add",
    "member_remove",
    "agent_add",
    "key_rotate",
    "members",
    "member_log",
    "member_alias",
    // transport / presence
    "status",
    "doctor",
    "my_id",
    "invite_ticket",
    "join_room",
    "connect",
    "who",
    "whoami",
    "sync",
];

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
pub struct MemberAliasArgs {
    /// A who-ref: a key id-prefix, a full key, or an existing alias.
    pub who: String,
    /// The petname to assign (empty string clears it).
    pub name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JoinArgs {
    /// A base32 space ticket from `invite_ticket`.
    pub ticket: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConnectArgs {
    /// A base32 space ticket from a coworker's `invite_ticket`.
    pub ticket: String,
}

fn mcp_string(value: &serde_json::Value, field: &str) -> Result<String, McpError> {
    mcp_string_opt(value, field)
        .ok_or_else(|| McpError::invalid_params(format!("Issues tool is missing '{field}'"), None))
}

fn mcp_string_opt(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
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
    /// (`lait members agent --new <name>`); MCP attaches as it thereafter.
    act_as: Option<String>,
    #[allow(dead_code)]
    tool_router: ToolRouter<LaitMcp>,
}

#[tool_router]
impl LaitMcp {
    pub fn new(home: PathBuf) -> Self {
        // `$LAIT_AGENT` names the sponsored local agent identity this MCP server
        // acts as, so its work is attributed to the agent (Architecture B). Unset
        // → the primary identity (pre-B behavior).
        let act_as = std::env::var("LAIT_AGENT").ok().filter(|s| !s.is_empty());
        let scope = scope_for_home(&home);
        let mut tool_router = Self::tool_router();
        tool_router.merge(Self::world_tool_router());
        Self {
            home,
            scope,
            act_as,
            tool_router,
        }
    }

    fn world_tool_router() -> ToolRouter<Self> {
        let registry = crate::world::client_packages();
        registry
            .validate_reserved(
                std::iter::empty::<&str>(),
                Self::tool_router()
                    .list_all()
                    .iter()
                    .map(|tool| tool.name.as_ref()),
            )
            .expect("World MCP tools must not collide with shell tools");
        let mut router = ToolRouter::new();
        for mounted in registry.mcp_tools() {
            let schema = mounted
                .tool
                .schema()
                .as_object()
                .cloned()
                .expect("World MCP input schema must be a JSON object");
            let tool = mounted.tool.clone();
            let route = ToolRoute::new_dyn(
                Tool::new(mounted.public_name, mounted.tool.description(), schema),
                move |context: rmcp::handler::server::tool::ToolCallContext<'_, Self>| {
                    let tool = tool.clone();
                    Box::pin(async move {
                        let input =
                            serde_json::Value::Object(context.arguments.unwrap_or_default());
                        let invocation = tool
                            .call(input)
                            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
                        context.service.run_invocation(invocation).await
                    })
                },
            );
            router.add_route(route);
        }
        router
    }

    /// Drive the daemon and return its `Response` as JSON text (the same
    /// versioned DTO the CLI `--json` emits).
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
        let response = client_as_scoped(&self.home, req, &self.scope, self.act_as.as_deref()).await;
        Self::tool_result(response)
    }

    async fn run_invocation(
        &self,
        invocation: world_interface::CliInvocation,
    ) -> Result<CallToolResult, McpError> {
        let response = match invocation {
            world_interface::CliInvocation::World(call) => {
                client_action_as_scoped(
                    &self.home,
                    ClientAction::world(call),
                    &self.scope,
                    self.act_as.as_deref(),
                )
                .await
            }
            world_interface::CliInvocation::Local { operation, input } => {
                let request = match operation.as_str() {
                    issues_app::cli::LOCAL_INBOX => Request::Inbox {
                        clear: input
                            .get("clear")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    },
                    issues_app::cli::LOCAL_ACCESS => {
                        let action = input
                            .get("action")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("ls");
                        match action {
                            "ls" => Request::AccessList {
                                actor: mcp_string_opt(&input, "actor"),
                            },
                            "grant" => Request::AccessGrant {
                                actor: mcp_string(&input, "actor")?,
                                role: mcp_string(&input, "role")?,
                                project: mcp_string_opt(&input, "project"),
                            },
                            "revoke" => Request::AccessRevoke {
                                grant_id: mcp_string(&input, "grant_id")?,
                            },
                            other => {
                                return Err(McpError::invalid_params(
                                    format!("unsupported Issues access action '{other}'"),
                                    None,
                                ));
                            }
                        }
                    }
                    other => {
                        return Err(McpError::invalid_params(
                            format!("unsupported World host capability '{other}'"),
                            None,
                        ));
                    }
                };
                client_as_scoped(&self.home, request, &self.scope, self.act_as.as_deref()).await
            }
        };
        Self::tool_result(response)
    }

    fn tool_result(response: anyhow::Result<Response>) -> Result<CallToolResult, McpError> {
        match response {
            Ok(Response::Error {
                message,
                error_kind,
            }) => Err(match error_kind {
                ErrorKind::Denied | ErrorKind::NotFound => McpError::invalid_request(message, None),
                ErrorKind::Error => McpError::internal_error(message, None),
            }),
            Ok(resp) => {
                let json = serde_json::to_string(&resp)
                    .unwrap_or_else(|_| "{\"kind\":\"ok\"}".to_string());
                Ok(CallToolResult::success(vec![Content::text(json)]))
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
        description = "Sponsor an agent keypair (any human member). The agent can read/write \
                       content but cannot manage membership or delete issues, and its standing \
                       dies with the sponsor."
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

    #[tool(
        description = "Set (or clear, with an empty name) a local petname for a key. Local to this device, never synced or part of the signed ACL."
    )]
    async fn member_alias(
        &self,
        Parameters(a): Parameters<MemberAliasArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(Request::MemberAlias {
            who: a.who,
            name: a.name,
        })
        .await
    }

    // ---- transport / presence ----

    #[tool(description = "Show node + space status: id, nick, space, issue/project counts.")]
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

    #[tool(description = "Get this node's endpoint id — the handle a coworker uses to reach us.")]
    async fn my_id(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Id).await
    }

    #[tool(
        description = "Produce a base32 space ticket to share so a coworker can join. The ticket carries a signed, single-use pass so they are auto-admitted on join (no separate approve step)."
    )]
    async fn invite_ticket(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Invite {
            role: None,
            reusable: false,
            ttl_hours: None,
        })
        .await
    }

    #[tool(
        description = "Connect to the bound space via a ticket for it and broadcast a request \
                       to be added. MCP runs against an already-bound store: a ticket for a \
                       different space errors (join it with the CLI first)."
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
                       and whether it is safe to author (partial_view must be false)."
    )]
    async fn whoami(&self) -> Result<CallToolResult, McpError> {
        self.run(Request::Whoami).await
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
}

#[tool_handler]
impl ServerHandler for LaitMcp {
    fn get_info(&self) -> ServerInfo {
        let product_instructions = crate::world::client_packages()
            .packages()
            .map(|package| package.mcp_instructions())
            .collect::<Vec<_>>()
            .join(" ");
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(format!(
                "A local-first, peer-to-peer issue tracker. You are a member of this space \
                 with your OWN identity — you do not rebuild or re-join per session; you \
                 attach. Start by calling whoami: it reports who you are (your actor + \
                 did:key), your role and capabilities, who sponsors you, and the space. If \
                 whoami shows you are not yet a member, a human runs `lait agent add <your \
                 device key>` once to sponsor you — then you hold write access and act as \
                 yourself (your work is attributed to you, not the human). Do NOT treat \
                 onboarding as invite→connect; that is the peer-JOIN flow for a new node, \
                 not for you. {product_instructions} File and drive issues with the namespaced \
                 tools: create with issues_new, edit with issues_edit, use \
                 issues_move/issues_assign/issues_label/issues_comment, and read with \
                 issues_list/issues_board/issues_view. Refs are a short iss_ handle or a KEY-n alias \
                 (ENG-142); @me is you. Before acting on a 'close what's done' style request, \
                 call sync — it converges and refuses to let you author against a known-partial \
                 view. Every tool returns the same versioned JSON DTO the CLI --json emits; \
                 a denied action tells you exactly what standing you lack and how to get it."
            ))
    }
}

/// Run the MCP server over stdio until the client disconnects.
pub async fn run_mcp(home: &Path) -> Result<()> {
    let service = LaitMcp::new(home.to_path_buf()).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use crate::daemon::OrbitAddress;
    use crate::ids::SpaceId;

    #[test]
    fn an_mcp_server_is_pinned_to_the_home_it_was_constructed_for() {
        let home = PathBuf::from("/tmp/lait-mcp-a");
        let sibling = PathBuf::from("/tmp/lait-mcp-b");
        let space = SpaceId::from_digest([9; 16]);
        let mcp = LaitMcp::new(home.clone());
        let own = OrbitAddress::for_store(&home, space.clone());
        let other = OrbitAddress::for_store(&sibling, space);

        assert!(mcp.scope.authorize(&own).is_ok());
        assert!(mcp.scope.authorize(&other).is_err());
    }

    #[test]
    fn the_live_router_composes_shell_and_namespaced_world_tools() {
        let mcp = LaitMcp::new(PathBuf::from("/tmp/lait-mcp-tools"));
        let names: Vec<_> = mcp
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        for expected in MCP_TOOL_NAMES {
            assert!(names.iter().any(|name| name == expected), "{expected}");
        }
        assert!(!names.iter().any(|name| name == "issue_new"));
        assert_eq!(names.len(), MCP_TOOL_NAMES.len());
    }
}
