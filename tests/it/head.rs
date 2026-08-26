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

/// A throwaway root for one test, removed when the test ends.
///
/// It has to be a guard rather than a path. The name carries the process id so
/// two tests never share one, which also means the `remove_dir_all` this used
/// to do on the way *in* could never match a previous run's directory — so
/// nothing ever deleted one. A head test installs both World fixtures under
/// here, so a full suite left gigabytes behind, every run, and the machine
/// filled up days later with directories named after tests that had long since
/// passed.
///
/// Removing on drop also covers the case that matters most: a test that
/// panics. It unwinds through here, and the root goes with it.
pub struct TempRoot {
    path: PathBuf,
}

impl std::ops::Deref for TempRoot {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempRoot {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl TempRoot {
    /// This root as a plain path.
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Adopt this root's canonical spelling.
    ///
    /// Windows' named pipe and the probe that looks for it have to agree on one
    /// spelling of the same directory — the trap `launcher_safety` documents.
    /// It is the same directory either way, so the guard still removes it.
    #[must_use]
    pub fn canonicalized(self) -> Self {
        #[cfg(windows)]
        {
            let path = lait::config::canonical(&self.path);
            // `self` still owns the original path and must not run its
            // destructor while a second guard holds the same directory.
            let held = std::mem::ManuallyDrop::new(self);
            let _ = &held;
            return TempRoot { path };
        }
        #[cfg(not(windows))]
        self
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        // A root that will not go is not worth failing a passing test over —
        // and on Windows it is usually a handle that closes a moment later.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A throwaway root for one test, named after it.
pub fn temp_root(tag: &str) -> TempRoot {
    // A counter as well as the pid: `cargo test` runs a whole target in one
    // process, so the pid alone collides between tests in the same binary.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("lait-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&path).ok();
    std::fs::create_dir_all(&path).expect("temp root");
    TempRoot { path }
}

/// Install the process fixtures through the same signed-channel and immutable
/// record boundary as a real client. The publisher is an explicit test input:
/// there is no fallback to package resources, a neighboring build directory,
/// or hand-authored installation records.
pub fn install_independent_test_worlds(identity: &Path) {
    let channels = std::env::var_os("WORLD_FIXTURE_CHANNELS").unwrap_or_else(|| {
        panic!(
            "WORLD_FIXTURE_CHANNELS is required; run \
             ci/prepare-independent-world-fixtures.sh with explicit built artifacts"
        )
    });
    let channels = PathBuf::from(channels);
    assert!(
        channels.is_dir(),
        "fixture channels are absent: {}",
        channels.display()
    );
    let encoded = std::fs::read_to_string(channels.join("pubkey.hex"))
        .expect("read the fixture channel public key");
    let decoded = data_encoding::HEXLOWER
        .decode(encoded.trim().as_bytes())
        .expect("decode the fixture channel public key");
    let pubkey: [u8; 32] = decoded
        .try_into()
        .expect("the fixture channel public key is 32 bytes");
    let installations = lait::serve::head::installations_root(identity);

    for world in ["com.lait.issues", "com.lait.signage"] {
        let outcome = lait::update::world::install_from_published_directory(
            &channels.join(world),
            &[pubkey],
            world,
            &lait::update::facts::offered(),
            &installations,
        )
        .expect("install the independently published fixture World");
        assert!(
            matches!(
                outcome,
                lait::update::world::Outcome::Staged { .. }
                    | lait::update::world::Outcome::Current { .. }
            ),
            "the signed fixture World was not installed: {outcome:?}"
        );
    }
}

/// The environment every head in these suites runs under.
///
/// `LAIT_CONFIG_ROOT` is the isolation that matters: `$LAIT_HOME` pins the
/// store, but the Orbit registry lives under the config root, so without this
/// every space founded here files itself in the developer's real catalog.
fn with_env(command: &mut Command, config: &Path, home: Option<&Path>) {
    install_independent_test_worlds(home.unwrap_or(config));
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
        .env("LAIT_IDLE_SECS", "0")
        // A test daemon never hosts the display coordinator: it binds one
        // well-known machine-scoped port, and this suite runs many daemons in
        // parallel — hosting would make them mutually exclusive with each
        // other and with any real daemon on the machine. The receiver suite
        // (tools/astrolabe/tests/launch.rs) is the one place that hosts.
        .env("LAIT_DISPLAY", "off");
    if let Some(home) = home {
        command.env("LAIT_HOME", home);
    }
}

/// The local app: `lait --json --port 0`, and the daemon under it.
pub struct Head {
    child: Child,
    config: PathBuf,
    home: Option<PathBuf>,
    /// Whether [`Head::stop`] already ran, so [`Drop`] does not redo it.
    stopped: bool,
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
        let port = banner["port"]
            .as_u64()
            .unwrap_or_else(|| panic!("readiness line carries no port: {banner}"));
        let token = banner["token"]
            .as_str()
            .unwrap_or_else(|| panic!("readiness line carries no token: {banner}"))
            .to_string();
        Head {
            child,
            config: config.to_path_buf(),
            home: home.map(Path::to_path_buf),
            stopped: false,
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

    /// This head's own process id, for a test whose subject is reaping.
    pub fn pid(&self) -> u32 {
        self.child.id()
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
        self.shut_down();
    }

    fn shut_down(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        let _ = self.child.kill();
        // `kill` only asks. Without the wait the child is a zombie, and on a
        // suite that starts one per test the reaping is the point.
        let _ = self.child.wait();
        stop_daemon(&self.config, self.home.as_deref());
    }
}

/// A test that panics must not leave a head — or the daemon under it — running.
///
/// `stop` is the graceful path and every passing test calls it. Nothing called
/// anything on the failing path, and `Child`'s own drop neither kills nor
/// reaps, so a single failed assertion orphaned a head, its daemon and every
/// World runner beneath. Those hold the display coordinator's fixed port, so
/// the *next* test failed too, and orphaned its own — one bad assertion turned
/// into a suite-wide cascade that read as flakiness.
impl Drop for Head {
    fn drop(&mut self) {
        self.shut_down();
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

/// Stop one daemon and wait for its control channel teardown to finish.
pub fn stop_daemon(config: &Path, home: Option<&Path>) {
    let daemon_home = daemon_home(config, home);
    #[cfg(unix)]
    let socket = lait::config::socket_path(&daemon_home);
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
        loop {
            let absent = matches!(
                lait::control::probe(&daemon_home).await,
                lait::control::Probe::Absent
            );
            // On Unix the listener stops accepting before the daemon finishes
            // draining placements and unlinks its pathname socket.  A failed
            // connect therefore proves only that the front door is shut; wait
            // for the unlink as the observable completion of graceful teardown.
            #[cfg(unix)]
            let cleaned = !socket.exists();
            #[cfg(not(unix))]
            let cleaned = true;
            if absent && cleaned {
                return;
            }
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
    /// `None` once the session has been ended: closing this pipe is the signal
    /// the server exits on, so shutting down has to be able to take it.
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// Whether [`Mcp::stop`] already ran, so [`Drop`] does not redo it.
    stopped: bool,
}

impl Mcp {
    /// The pipe the server reads, while the session is open.
    fn writer(&mut self) -> &mut ChildStdin {
        self.stdin
            .as_mut()
            .expect("the MCP session was already ended")
    }
}

impl Mcp {
    /// Start `lait mcp` on the Orbit `$LAIT_HOME` selects, optionally acting as
    /// a sponsored agent.
    pub fn start(config: &Path, home: &Path, agent: Option<&str>) -> Mcp {
        let mut command = Command::new(bin());
        with_env(&mut command, config, Some(home));
        // The editor binding authors the World pin — `lait mcp` refuses an
        // ambiguous mount now that the build hosts more than one World, and
        // this harness models the binding, so it pins the way a real one does.
        command.env("LAIT_WORLD", "issues");
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
            stdin: Some(stdin),
            stdout,
            next_id: 0,
            stopped: false,
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
            self.writer(),
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
            self.writer(),
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
        )
        .expect("write request");
        self.writer().flush().ok();
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
        if reply["result"]["isError"] == true {
            panic!("{tool} was a tool error: {reply}");
        }
        if !reply["result"]["structuredContent"].is_null() {
            return reply["result"]["structuredContent"].clone();
        }
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
    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        // Closing stdin is what tells the server to flush and exit, so it has
        // to go before the wait — otherwise this waits on a process that is
        // still waiting on us.
        self.stdin.take();
        let _ = self.child.wait();
    }
}

/// The same reaping [`Head`] needs, for the same reason: a panicking test
/// never reaches `stop`, and `Child`'s drop neither closes stdin nor waits.
impl Drop for Mcp {
    fn drop(&mut self) {
        self.shut_down();
    }
}
