//! `lait install-mcp`: register the MCP server with an agent's config in
//! one explicit step, instead of hand-editing JSON. Merges into the target
//! client's `mcpServers` block without clobbering other servers.

use std::{fs, path::PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};

/// MCP-speaking agent whose config we know how to write.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
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

/// Where to write the config: shared across a machine, or local to a project.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
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
fn config_path(client: Client, scope: Scope) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("get current dir")?;
    Ok(match (client, scope) {
        (Client::Generic, _) | (Client::Claude, Scope::Project) => cwd.join(".mcp.json"),
        (Client::Claude, Scope::User) => home()?.join(".claude.json"),
        (Client::Cursor, Scope::Project) => cwd.join(".cursor").join("mcp.json"),
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
/// Orbit exactly as the CLI does — walking up from the client's working
/// directory for a `.lait/` — so one entry serves every space on the machine
/// and never needs repointing.
///
/// Both of the things this *stopped* doing were silent-failure generators. A
/// pinned absolute path goes stale the moment the binary moves or the control
/// protocol advances (the daemon handshake then refuses a CLI whose version
/// string is unchanged). A captured `$LAIT_HOME` outlives the shell that set
/// it, and because a home is created on demand it resolves to a freshly-made
/// empty directory — reported as "no local Orbit here", which reads like a
/// broken store rather than a stale config.
fn server_entry(agent: Option<&str>) -> Value {
    let mut entry = Map::new();
    entry.insert("command".into(), json!("lait"));
    entry.insert("args".into(), json!(["mcp"]));
    if let Some(a) = agent {
        entry.insert("env".into(), json!({ "LAIT_AGENT": a }));
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

/// Register (or update) the lait MCP server in `client`'s config. With
/// `print`, returns the would-be file contents instead of writing.
pub fn install_mcp(
    client: Client,
    scope: Option<Scope>,
    name: &str,
    agent: Option<&str>,
    print: bool,
) -> Result<String> {
    let scope = scope.unwrap_or_else(|| default_scope(client));
    let path = config_path(client, scope)?;

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
    servers.insert(name.to_string(), server_entry(agent));

    let pretty = serde_json::to_string_pretty(&root)? + "\n";
    if print {
        // stdout stays the file and nothing else, so `--print` remains pipeable;
        // the caveat still has to reach a human previewing the change.
        if let Some(note) = advice(client, name) {
            eprintln!("{note}\n");
        }
        return Ok(pretty);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, &pretty).with_context(|| format!("write {}", path.display()))?;

    let mut msg = format!(
        "{} MCP server '{}' in {}\nRestart your agent (or reload its MCP servers) to pick it up.",
        if existed { "updated" } else { "added" },
        name,
        path.display()
    );
    match agent {
        // Naming an agent is the whole of Architecture B from the config side;
        // the identity itself is still a deliberate, human-sponsored act.
        Some(a) => msg.push_str(&format!(
            "\n\nWork will be attributed to the agent identity '{a}'. Provision it once with:\n  \
             lait members agent --new {a}"
        )),
        None => msg.push_str(
            "\n\nWork will be attributed to you, not to the agent. Pass --agent <name> to sign \
             its work\nas a sponsored identity of its own.",
        ),
    }
    if let Some(note) = advice(client, name) {
        msg.push_str("\n\n");
        msg.push_str(&note);
    }
    Ok(msg)
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
        let e = server_entry(None);
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
        let e = server_entry(Some("claude"));
        assert_eq!(e["env"], json!({ "LAIT_AGENT": "claude" }));
        assert_eq!(e["command"], json!("lait"));
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
