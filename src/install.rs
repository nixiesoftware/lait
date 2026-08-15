//! Register the lait MCP server with an agent's config in one explicit step,
//! instead of hand-editing JSON. Merges into the target client's `mcpServers`
//! block without clobbering other servers.
//!
//! Reached as [`crate::control::Request::HostInstallMcp`]. It has to live on the
//! host plane rather than in a head: a head cannot write the file that tells an
//! agent how to reach it, and the write must work before any store exists.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// MCP-speaking agent whose config we know how to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Client {
    /// Claude Code (`.mcp.json` project, `~/.claude.json` user).
    Claude,
    /// Cursor (`.cursor/mcp.json`).
    Cursor,
    /// Windsurf (`~/.codeium/windsurf/mcp_config.json`, global only).
    Windsurf,
    /// Any client that reads a `.mcp.json` in the working directory.
    Generic,
}

impl Client {
    /// The agent identity a client signs its work as, by default.
    ///
    /// Naming the client already names the agent, so `--client claude` is enough
    /// to get attribution: the tools act as a sponsored member called `claude`
    /// rather than as the human whose home hosts the daemon. The name is also
    /// what the browser draws the agent by — a local petname matching a known
    /// coding tool gets that tool's brand mark (`viewer/src/ui/agentLogos.ts`),
    /// so a client-derived name is what makes an agent legible as itself instead
    /// of as an unnamed key.
    ///
    /// `Generic` has no native name to derive — the caller must say who it is.
    const fn agent_name(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude"),
            Self::Cursor => Some("cursor"),
            Self::Windsurf => Some("windsurf"),
            Self::Generic => None,
        }
    }
}

/// Where to write the config: shared across a machine, or local to a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    User,
    Project,
}

fn home() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .ok_or_else(|| anyhow!("could not determine home directory"))
}

/// Sensible default scope per client (Windsurf only has a global config).
fn default_scope(client: Client) -> Scope {
    match client {
        Client::Windsurf => Scope::User,
        _ => Scope::Project,
    }
}

/// Resolve the config file for a client + scope.
///
/// `project` is passed in rather than read from `current_dir()`: this runs in
/// the identity's daemon, whose working directory has nothing to do with the
/// project the caller means.
fn config_path(client: Client, scope: Scope, project: &Path) -> Result<PathBuf> {
    Ok(match (client, scope) {
        (Client::Generic, _) | (Client::Claude, Scope::Project) => project.join(".mcp.json"),
        (Client::Claude, Scope::User) => home()?.join(".claude.json"),
        (Client::Cursor, Scope::Project) => project.join(".cursor").join("mcp.json"),
        (Client::Cursor, Scope::User) => home()?.join(".cursor").join("mcp.json"),
        (Client::Windsurf, _) => home()?
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json"),
    })
}

/// Build the `mcpServers` entry.
///
/// Deliberately portable: `lait` off PATH rather than a snapshot of
/// `current_exe()`, and no `$LAIT_HOME` capture. The server then discovers its
/// Orbit the way every head does — walking up from the client's working
/// directory for a `.lait/` — so one entry serves every space on the machine
/// and never needs repointing.
///
/// Both of the things this *stopped* doing were silent-failure generators. A
/// pinned absolute path goes stale the moment the binary moves or the control
/// protocol advances (the daemon handshake then refuses a client whose version
/// string is unchanged). A captured `$LAIT_HOME` outlives the shell that set
/// it, and because a home is created on demand it resolves to a freshly-made
/// empty directory — reported as "no local Orbit here", which reads like a
/// broken store rather than a stale config.
fn server_entry(agent: Option<&str>, world: Option<&str>) -> Value {
    let mut entry = Map::new();
    entry.insert("command".into(), json!("lait"));
    entry.insert("args".into(), json!(["mcp"]));
    let mut env = Map::new();
    if let Some(a) = agent {
        env.insert("LAIT_AGENT".into(), json!(a));
    }
    if let Some(world) = world.filter(|world| !world.is_empty()) {
        env.insert("LAIT_WORLD".into(), json!(world));
    }
    if !env.is_empty() {
        entry.insert("env".into(), Value::Object(env));
    }
    Value::Object(entry)
}

/// Client-specific note appended to the success message. `--client` is required
/// precisely so this can be accurate: the shapes are portable and identical,
/// but what a written entry *means* differs by client.
fn advice(client: Client, name: &str) -> Option<String> {
    match client {
        // The bundled Claude Code plugin already declares this server. A second
        // declaration under the same name shadows it, which is how a plugin
        // that needs no configuration acquires configuration that can rot.
        Client::Claude => Some(format!(
            "Note: the lait Claude Code plugin already provides an MCP server named 'lait'.\n\
             If you use the plugin, you do not need this entry — and a server named '{name}'\n\
             will shadow the plugin's. Install it only for a Claude Code without the plugin."
        )),
        Client::Windsurf => {
            Some("Note: Windsurf reads one global config; there is no project scope.".into())
        }
        Client::Cursor | Client::Generic => None,
    }
}

/// What an install produced, for whichever surface asked for it.
pub struct Installed {
    /// The config file this landed in — or, under `print`, would have.
    pub path: PathBuf,
    /// The `mcpServers` entry that would be written under `print`, else the
    /// human summary.
    pub detail: String,
    /// The client-specific caveat, when there is one.
    pub note: Option<String>,
    /// Whether an entry under this name already existed. Always `false` under
    /// `print`, which never opens the file.
    pub replaced: bool,
    /// The agent identity the written entry signs its work as.
    pub agent: Option<String>,
}

/// Register (or update) the lait MCP server in `client`'s config. With
/// `print`, returns the entry that would be written — and touches nothing.
pub fn install_mcp(
    client: Client,
    scope: Option<Scope>,
    name: &str,
    agent: Option<&str>,
    no_agent: bool,
    print: bool,
    project: &Path,
    world: Option<&str>,
) -> Result<Installed> {
    let scope = scope.unwrap_or_else(|| default_scope(client));
    let path = config_path(client, scope, project)?;
    // The named client picks its own agent identity; `--agent` overrides it and
    // `--no-agent` declines one, leaving the work signed by the human.
    let agent = if no_agent {
        None
    } else {
        agent.or_else(|| client.agent_name())
    };
    let note = advice(client, name);

    // `print` answers "what would you write", and the answer is the entry — not
    // the file. Merging into the file first and returning the result handed the
    // caller the whole of whatever config already sat at `path`, and `path` is
    // caller-directed: `project` is deliberately unadmitted, because this verb
    // targets an editor's project directory, which need not hold a store this
    // daemon serves. A dry run that reads is a read primitive for any JSON file
    // this process can open. The entry below is built from the request alone, so
    // it discloses nothing the caller did not already send.
    if print {
        let entry = json!({ "mcpServers": { name: server_entry(agent, world) } });
        return Ok(Installed {
            path,
            detail: serde_json::to_string_pretty(&entry)? + "\n",
            note,
            replaced: false,
            agent: agent.map(ToString::to_string),
        });
    }

    let mut root: Value = if path.exists() {
        let data = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if data.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&data).with_context(|| format!("parse {}", path.display()))?
        }
    } else {
        json!({})
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} is not a JSON object", path.display()))?;
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| anyhow!("mcpServers in {} is not an object", path.display()))?;
    let existed = servers.contains_key(name);
    servers.insert(name.to_string(), server_entry(agent, world));

    let pretty = serde_json::to_string_pretty(&root)? + "\n";
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, &pretty).with_context(|| format!("write {}", path.display()))?;

    let mut detail = format!(
        "{} MCP server '{}' in {}\nRestart your agent (or reload its MCP servers) to pick it up.",
        if existed { "updated" } else { "added" },
        name,
        path.display()
    );
    match agent {
        // Naming an agent is the whole of Architecture B from the config side;
        // the identity itself is still a deliberate, human-sponsored act.
        Some(a) => {
            use std::fmt::Write;
            let _ = write!(
                detail,
                "\n\nWork will be attributed to the agent identity '{a}'. The agent's first \
                 whoami asks the person on this machine (Astrolabe) to sponsor it."
            );
        }
        None => detail.push_str(
            "\n\nWork will be attributed to you, not to the agent. Name an agent to sign its \
             work\nas a sponsored identity of its own.",
        ),
    }
    Ok(Installed {
        path,
        detail,
        note,
        replaced: existed,
        agent: agent.map(ToString::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The written entry must stay portable. Both regressions this guards
    /// against shipped once: an absolute `current_exe()` that outlived the
    /// binary it named, and a `$LAIT_HOME` snapshot that outlived the shell
    /// that set it and then resolved to an empty directory.
    #[test]
    fn entry_pins_nothing_machine_specific() {
        let e = server_entry(None, None);
        assert_eq!(e["command"], json!("lait"));
        assert_eq!(e["args"], json!(["mcp"]));
        assert!(
            e.get("env").is_none(),
            "no agent named, so no env block at all: {e}"
        );
        assert!(
            !e.to_string().contains("LAIT_HOME"),
            "must never capture a home: {e}"
        );
        let cmd = e["command"].as_str().expect("command is a string");
        assert!(
            !cmd.contains(['/', '\\', ':']),
            "must be a bare PATH lookup, not a path: {cmd}"
        );
    }

    #[test]
    fn naming_an_agent_adds_only_that() {
        let e = server_entry(Some("claude"), None);
        assert_eq!(e["env"], json!({ "LAIT_AGENT": "claude" }));
        assert_eq!(e["command"], json!("lait"));
    }

    #[test]
    fn naming_a_world_adds_the_pin_and_nothing_machine_specific() {
        let e = server_entry(Some("claude"), Some("signage"));
        assert_eq!(
            e["env"],
            json!({ "LAIT_AGENT": "claude", "LAIT_WORLD": "signage" })
        );
        assert!(
            !e.to_string().contains("LAIT_HOME"),
            "must never capture a home: {e}"
        );
    }

    /// Naming the client names the agent. The identity is what the browser draws
    /// an agent by, so a client that derives one is the difference between an
    /// agent that appears as itself and one that appears as an unnamed key.
    #[test]
    fn each_known_client_brings_its_own_agent_identity() {
        assert_eq!(Client::Claude.agent_name(), Some("claude"));
        assert_eq!(Client::Cursor.agent_name(), Some("cursor"));
        assert_eq!(Client::Windsurf.agent_name(), Some("windsurf"));
        // Nothing to derive from, so the caller has to say who it is rather than
        // have a wrong name chosen for them.
        assert_eq!(Client::Generic.agent_name(), None);
    }

    /// The agent names shipped above are the ones the viewer's logo table knows
    /// (`viewer/src/ui/agentLogos.ts`); a rename on either side that silences the
    /// brand mark should be a deliberate, visible one.
    #[test]
    fn derived_agent_names_stay_lowercase_plain_identifiers() {
        for client in [Client::Claude, Client::Cursor, Client::Windsurf] {
            let name = client.agent_name().expect("a native name");
            assert!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                // It also becomes a directory segment under the home.
                "{name} must be a plain lowercase identifier"
            );
        }
    }

    /// A dry run must not be a way to read.
    ///
    /// **The failure this prevents:** the target directory is caller-directed
    /// and deliberately unadmitted — this verb aims at an editor's project,
    /// which need not hold a store this daemon serves — so a `print` that merged
    /// the file at that path into its answer handed back any JSON this process
    /// could open, to anything holding the loopback token.
    #[test]
    fn printing_never_returns_the_file_it_would_have_written() {
        let dir = std::env::temp_dir().join(format!("lait-mcp-print-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("project dir");
        let planted =
            r#"{"mcpServers":{"other":{"command":"another-binary"}},"authToken":"not-yours"}"#;
        fs::write(dir.join(".mcp.json"), planted).expect("plant a config");

        let printed = install_mcp(
            Client::Generic,
            Some(Scope::Project),
            "lait",
            Some("scout"),
            false,
            true,
            &dir,
            None,
        )
        .expect("print");

        assert_eq!(printed.path, dir.join(".mcp.json"));
        assert!(
            !printed.detail.contains("not-yours") && !printed.detail.contains("another-binary"),
            "print disclosed the file it would touch: {}",
            printed.detail
        );
        // What it does answer is the entry, in full — built from the request, so
        // it says nothing back the caller did not already send.
        let entry: Value = serde_json::from_str(&printed.detail).expect("print answers JSON");
        assert_eq!(entry["mcpServers"]["lait"]["command"], json!("lait"));
        assert_eq!(
            entry["mcpServers"]["lait"]["env"]["LAIT_AGENT"],
            json!("scout")
        );
        assert_eq!(
            fs::read_to_string(dir.join(".mcp.json")).expect("read back"),
            planted,
            "print must write nothing"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Claude Code ships the server in its plugin, so a written entry shadows
    /// it. Saying so is the reason `--client` is required rather than defaulted.
    #[test]
    fn claude_warns_about_shadowing_the_plugin() {
        let note = advice(Client::Claude, "lait").expect("claude gets a note");
        assert!(note.contains("plugin"), "{note}");
        assert!(note.contains("shadow"), "{note}");
        assert!(advice(Client::Cursor, "lait").is_none());
    }
}
