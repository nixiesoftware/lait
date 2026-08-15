//! Driving lait the way anything drives it now: through a head.
//!
//! There is no command surface, so an integration test cannot type a verb. It
//! starts one of the two heads the binary offers and speaks that head's
//! protocol — loopback HTTP for the local app, JSON-RPC over stdio for MCP.
//! Both are the *real* deployed shapes: the HTTP bytes below are the ones
//! `viewer/scripts/dev.mjs` speaks, and the stdio frames are the ones an agent
//! client sends.

#![allow(dead_code, reason = "one harness, many suites; each uses a subset")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_lait")
}

/// A throwaway root for one test, named after it.
pub fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lait-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).expect("temp root");
    root
}

/// The environment every head in these suites runs under.
///
/// `LAIT_CONFIG_ROOT` is the isolation that matters: `$LAIT_HOME` pins the
/// store, but the Orbit registry lives under the config root, so without this
/// every space founded here files itself in the developer's real catalog.
fn with_env(command: &mut Command, config: &Path, home: Option<&Path>) {
    command
        .env_remove("LAIT_HOME")
        .env_remove("LAIT_STORE")
        .env_remove("LAIT_AGENT")
        .env_remove("LAIT_AS")
        .env("LAIT_CONFIG_ROOT", config)
        .env("LAIT_NETWORK", "isolated")
        // A daemon auto-started for a test otherwise lingers for the 30-minute
        // idle window, and a client that connects while one is tearing down can
        // park. Tests must not race that.
        .env("LAIT_IDLE_SECS", "0");
    if let Some(home) = home {
        command.env("LAIT_HOME", home);
    }
}

/// The local app: `lait --json --port 0`, and the daemon under it.
pub struct Head {
    child: Child,
    config: PathBuf,
    home: Option<PathBuf>,
    pub port: u16,
    pub token: String,
}

impl Head {
    /// Start the head and wait for its readiness line.
    ///
    /// `--port 0` binds an ephemeral port so concurrent suites never collide.
    /// The first stdout line is the contract `dev.mjs` parses: `{url, token,
    /// port}`, emitted before the listener starts accepting.
    pub fn start(config: &Path, home: Option<&Path>) -> Head {
        let mut command = Command::new(bin());
        with_env(&mut command, config, home);
        let mut child = command
            .args(["--json", "--port", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the lait head");

        let mut line = String::new();
        let mut reader = BufReader::new(child.stdout.take().expect("head stdout"));
        reader
            .read_line(&mut line)
            .expect("read the readiness line");
        let banner: serde_json::Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|error| panic!("readiness line is not JSON ({error}): {line}"));
        let port = banner["port"].as_u64().expect("port");
        let token = banner["token"].as_str().expect("token").to_string();
        Head {
            child,
            config: config.to_path_buf(),
            home: home.map(Path::to_path_buf),
            #[allow(clippy::as_conversions, reason = "a bound TCP port is a u16")]
            port: port as u16,
            token,
        }
    }

    /// One HTTP POST against this head. Written by hand rather than with a
    /// client crate: it is one request, and these bytes are also the contract
    /// `dev.mjs` and any embedding editor plugin speak.
    pub fn post(&self, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let (status, raw) = self.post_raw(path, &self.token, &body.to_string());
        let parsed = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        (status, parsed)
    }

    pub fn post_raw(&self, path: &str, token: &str, body: &str) -> (u16, String) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect to the lait head");
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\
             Content-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            port = self.port,
            len = body.len()
        );
        stream.write_all(request.as_bytes()).expect("write request");
        stream.flush().ok();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read response");
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    pub fn get(&self, path: &str) -> (u16, serde_json::Value) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect to the lait head");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\
             Connection: close\r\n\r\n",
            port = self.port,
            token = self.token,
        );
        stream.write_all(request.as_bytes()).expect("write request");
        stream.flush().ok();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read response");
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        (
            status,
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    /// One host-plane request: the plane that answers before any Orbit exists.
    pub fn host(&self, body: serde_json::Value) -> (u16, serde_json::Value) {
        self.post("/api/host/rpc", &body)
    }

    /// One Space control request against an Orbit.
    pub fn space(&self, orbit: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        self.post(&format!("/api/spaces/{orbit}/rpc"), &body)
    }

    /// One World request. `confirm` answers a destructive verb's question.
    pub fn world(
        &self,
        orbit: &str,
        world: &str,
        body: serde_json::Value,
        confirm: bool,
    ) -> (u16, serde_json::Value) {
        let query = if confirm { "?confirm=true" } else { "" };
        self.post(
            &format!("/api/spaces/{orbit}/worlds/{world}/rpc{query}"),
            &body,
        )
    }

    /// Found a Space into `dir`, and return its local Orbit id.
    pub fn found(&self, dir: &Path, name: &str) -> String {
        let store = dir.join(".lait");
        let (status, reply) = self.host(serde_json::json!({
            "cmd": "host_space_found",
            "home": store.display().to_string(),
            "name": name,
            "nick": "test",
        }));
        assert_eq!(status, 200, "found {name}: {reply}");
        assert_eq!(reply["host"], "founded", "found {name}: {reply}");
        self.orbit_for(&store)
    }

    /// The local Orbit id the catalog lists for a store path.
    pub fn orbit_for(&self, store: &Path) -> String {
        let (status, listing) = self.get("/api/spaces");
        assert_eq!(status, 200, "list spaces: {listing}");
        let want = canonical(store);
        listing["spaces"]
            .as_array()
            .expect("spaces array")
            .iter()
            .find(|row| canonical(Path::new(row["path"].as_str().unwrap_or_default())) == want)
            .and_then(|row| row["id"].as_str())
            .unwrap_or_else(|| panic!("no catalog row for {}: {listing}", store.display()))
            .to_string()
    }

    /// Is the head still up?
    ///
    /// `try_wait` polls rather than blocks, and reaps if the head has already
    /// gone — which matters to any test whose subject is process reaping.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stop the head, then the daemon it started.
    pub fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        stop_daemon(&self.config, self.home.as_deref());
    }
}

/// One spelling of a path, so two of them can be compared.
///
/// The registry stores the canonical form with Windows' extended-length `\\?\`
/// prefix stripped (`config::canonical`), which is a *different string* from
/// what `fs::canonicalize` hands back here — comparing the two raw would miss
/// every row on Windows.
pub fn canonical(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => resolved,
    }
}

/// The daemon home for a config root and optional self-contained home.
pub fn daemon_home(config: &Path, home: Option<&Path>) -> PathBuf {
    match home {
        Some(home) => canonical(home).join("daemon"),
        None => config.join("daemon"),
    }
}

/// Stop one daemon and wait for its control channel to go quiet.
pub fn stop_daemon(config: &Path, home: Option<&Path>) {
    let daemon_home = daemon_home(config, home);
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let client = lait::daemon::Client::at(daemon_home.clone());
        let _ = client
            .request(
                lait::control::ControlRoute::Daemon,
                &lait::control::Request::Stop,
                None,
            )
            .await;
        let deadline = Instant::now() + Duration::from_secs(15);
        while !matches!(
            lait::control::probe(&daemon_home).await,
            lait::control::Probe::Absent
        ) {
            if Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

/// The stdio MCP head, mid-session.
///
/// The one head that can act as somebody other than the human whose daemon it
/// talks to: `$LAIT_AGENT` names the sponsored identity every tool call is
/// signed and attributed as.
pub struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    /// Start `lait mcp` on the Orbit `$LAIT_HOME` selects, optionally acting as
    /// a sponsored agent.
    pub fn start(config: &Path, home: &Path, agent: Option<&str>) -> Mcp {
        let mut command = Command::new(bin());
        with_env(&mut command, config, Some(home));
        if let Some(agent) = agent {
            command.env("LAIT_AGENT", agent);
        }
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lait mcp");
        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = BufReader::new(child.stdout.take().expect("mcp stdout"));
        let mut mcp = Mcp {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        mcp.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "it", "version": "0" },
            }),
        );
        mcp.notify("notifications/initialized");
        mcp
    }

    fn notify(&mut self, method: &str) {
        writeln!(
            self.stdin,
            "{}",
            serde_json::json!({ "jsonrpc": "2.0", "method": method })
        )
        .expect("write notification");
    }

    /// One JSON-RPC request/response pair. Notifications and log lines in
    /// between are skipped: a reply is the frame carrying our id.
    pub fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        writeln!(
            self.stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
        )
        .expect("write request");
        self.stdin.flush().ok();
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read mcp stdout");
            assert!(read > 0, "the MCP server closed before answering {method}");
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if value["id"].as_u64() == Some(id) {
                return value;
            }
        }
    }

    /// Call one tool and return its parsed JSON payload, failing loudly with the
    /// MCP error when the call was refused.
    pub fn call_raw(&mut self, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        )
    }

    /// Call one tool and return its parsed JSON payload, failing loudly with the
    /// MCP error when the call was refused.
    pub fn call(&mut self, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        let reply = self.call_raw(tool, arguments);
        assert!(
            reply.get("error").is_none(),
            "{tool} was refused: {}",
            reply["error"]
        );
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{tool} returned no text content: {reply}"));
        serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.to_string()))
    }

    /// The names the server actually serves.
    pub fn tool_names(&mut self) -> Vec<String> {
        let reply = self.request("tools/list", serde_json::json!({}));
        reply["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name").to_string())
            .collect()
    }

    /// End the session. Dropping stdin is what tells the server to flush and
    /// exit, so it has to go before the wait.
    pub fn stop(self) {
        let Mcp {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        let _ = child.wait();
    }
}
