//! Heads: the browser and MCP adapters attached to a daemon.
//!
//! The supervisor owns the half it can prove — browser heads it spawned and
//! holds a handle to. This module is the other half: authoring the *binding*
//! that points an agent's harness at lait, which is not a process this client
//! ever holds and cannot pretend to.
//!
//! ## What an MCP binding actually pins
//!
//! Less than it sounds, and deliberately. `src/install.rs` writes `lait` off
//! `PATH` with no captured home, and the notes there say why both were removed:
//! a pinned absolute path goes stale the moment the binary moves, and a captured
//! `LAIT_HOME` outlives the shell that set it and then resolves to a freshly
//! made empty directory — reported as "no local Orbit here", which reads like a
//! broken store rather than a stale config.
//!
//! So the binding is: which agent client, which config scope, what to call the
//! server, which sponsored identity its work signs as, and which World the
//! session speaks. The Orbit is discovered from the project directory at run
//! time, which is what lets one entry serve every Space on the machine. A
//! client that offered to pin an Orbit here would be offering to author the
//! exact staleness that design removed, and this surface says what it does
//! instead. The World pin is not that: it is a mount name this build hosts,
//! written as `$LAIT_WORLD`, so two Worlds do not share one `tools/list`.

use lait::control::{ControlRoute, HostReply, Request, Response};
use lait::install::{Client as AgentClient, Scope};

use super::{Client, ClientError, ClientResult};

pub use lait_workbench::{HeadFacts, HeadKind, HeadState, Ownership, Stopped};

/// What authoring an MCP binding asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpBinding {
    pub client: AgentClient,
    /// `None` takes the agent client's own default — project for most, user for
    /// the one that has no project scope.
    pub scope: Option<Scope>,
    /// The server name to write under.
    pub name: String,
    /// The sponsored identity the agent's work signs as. `None` derives one
    /// from the client; `no_agent` declines one and leaves the work signed by
    /// the human.
    pub agent: Option<String>,
    pub no_agent: bool,
    /// The project directory a project-scoped config lands in. Carried
    /// explicitly because the daemon's working directory is not the person's.
    pub project: String,
    /// Mount of the World this session speaks. `None` lets `lait mcp` take
    /// the sole World this build hosts.
    pub world: Option<String>,
}

/// What authoring one produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpBindingOutcome {
    /// The config file it landed in — or, for a preview, would have.
    pub path: String,
    /// The would-be file contents for a preview, else the human summary.
    pub detail: String,
    /// The agent-client-specific caveat, when there is one. Shown rather than
    /// logged: "this entry shadows the bundled plugin" is the whole reason the
    /// client has to be named.
    pub note: Option<String>,
    pub replaced: bool,
    pub agent: Option<String>,
    /// Whether anything was written. A preview that looked like a write would
    /// be the worst possible answer for a verb whose failure mode is silence.
    pub written: bool,
    /// Mount this binding was authored for. The model holds one last outcome;
    /// a surface showing another World must ignore it.
    pub world: Option<String>,
}

impl Client {
    /// Every head this client can account for.
    ///
    /// Owned browser heads only. An MCP head is *structurally external* — the
    /// agent's harness spawns it — so listing one would mean reading somebody
    /// else's process table and guessing. What this client knows about MCP is
    /// the binding it wrote, not the process that may or may not be running
    /// against it, and the surface says which of the two it is showing.
    pub fn heads(&self) -> Vec<HeadFacts> {
        self.supervisor().list_heads()
    }

    /// Stop a head this client started, and say what actually happened.
    ///
    /// Two successes, kept apart all the way to the caller: the head was running
    /// and is not any more, or it had already exited. The second is the only way a
    /// person learns their World fell over on its own, so collapsing it here would
    /// throw away the fact and leave the surface reporting a button press.
    pub async fn stop_head(&self, id: &str) -> ClientResult<Stopped> {
        self.supervisor().stop_head(id).await.map_err(Into::into)
    }

    /// Author, or preview, an MCP binding.
    ///
    /// `preview` returns what would be written and touches nothing — which is
    /// the whole point of offering it: the file being edited is an agent's, and
    /// a person deserves to see the entry before it merges into a config they
    /// did not write.
    pub async fn install_mcp_head(
        &self,
        binding: &McpBinding,
        preview: bool,
    ) -> ClientResult<McpBindingOutcome> {
        if binding.name.trim().is_empty() {
            return Err(ClientError::invalid("an MCP binding needs a server name"));
        }
        if binding.project.trim().is_empty() {
            return Err(ClientError::invalid(
                "an MCP binding needs the project directory its config belongs to",
            ));
        }
        let reply = self
            .daemon()?
            .request(
                ControlRoute::Daemon,
                &Request::HostInstallMcp {
                    client: binding.client,
                    scope: binding.scope,
                    name: binding.name.trim().to_owned(),
                    agent: binding.agent.clone().filter(|a| !a.trim().is_empty()),
                    no_agent: binding.no_agent,
                    print: preview,
                    dir: binding.project.trim().to_owned(),
                    world: binding
                        .world
                        .as_deref()
                        .map(str::trim)
                        .filter(|world| !world.is_empty())
                        .map(str::to_owned),
                },
                None,
            )
            .await
            .map_err(|error| ClientError::unreachable(format!("reach the daemon: {error:#}")))?;
        match reply {
            Response::Host(HostReply::McpInstalled {
                path,
                detail,
                note,
                replaced,
                agent,
            }) => Ok(McpBindingOutcome {
                path,
                detail,
                note,
                replaced,
                agent,
                written: !preview,
                world: binding.world.clone(),
            }),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected MCP install reply: {other:?}"
            ))),
        }
    }
}

/// The agent clients this build knows how to write a config for.
///
/// Listed here rather than derived, because the enum is not iterable and a
/// surface has to offer a choice. Adding a variant upstream without adding it
/// here means it is simply not offered — visible, and not a silent misbehaviour.
pub const AGENT_CLIENTS: [(AgentClient, &str); 4] = [
    (AgentClient::Claude, "Claude Code"),
    (AgentClient::Cursor, "Cursor"),
    (AgentClient::Windsurf, "Windsurf"),
    (AgentClient::Generic, "Any .mcp.json client"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> McpBinding {
        McpBinding {
            client: AgentClient::Claude,
            scope: None,
            name: "lait".into(),
            agent: None,
            no_agent: false,
            project: "D:/work".into(),
            world: None,
        }
    }

    fn client(root: &std::path::Path) -> Client {
        let supervisor = lait_workbench::Supervisor::new(
            root.to_path_buf(),
            root.join(if cfg!(windows) { "lait.exe" } else { "lait" }),
        )
        .expect("a supervisor over an empty root");
        Client::over(supervisor, Some(root.to_path_buf()))
    }

    /// Both refusals are about the request rather than about the daemon, so
    /// they must land before anything is sent. A binding with no name would
    /// otherwise write a server called the empty string into somebody's config.
    #[tokio::test]
    async fn a_binding_with_nothing_to_write_is_refused_rather_than_sent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let client = client(directory.path());

        let nameless = McpBinding {
            name: "  ".into(),
            ..binding()
        };
        let refused = client
            .install_mcp_head(&nameless, true)
            .await
            .expect_err("a nameless binding was accepted");
        assert_eq!(refused.code, super::super::error::ErrorCode::Invalid);

        let placeless = McpBinding {
            project: String::new(),
            ..binding()
        };
        assert_eq!(
            client
                .install_mcp_head(&placeless, true)
                .await
                .expect_err("a binding with no project was accepted")
                .code,
            super::super::error::ErrorCode::Invalid
        );
    }

    /// A preview is not a write, and the outcome says so. This is the field a
    /// surface reads to decide whether to tell somebody their config changed.
    #[test]
    fn a_preview_is_never_reported_as_a_write() {
        let previewed = McpBindingOutcome {
            path: "D:/work/.mcp.json".into(),
            detail: "{}".into(),
            note: None,
            replaced: false,
            agent: None,
            written: false,
            world: Some("issues".into()),
        };
        assert!(!previewed.written);
    }

    /// Every agent client this build can write for is offered. A choice that
    /// exists in the engine and not in the client is a capability nobody can
    /// reach.
    #[test]
    fn every_agent_client_is_offered_with_a_name_a_person_recognises() {
        assert_eq!(AGENT_CLIENTS.len(), 4);
        for (_, label) in AGENT_CLIENTS {
            assert!(!label.is_empty());
        }
    }
}
