//! CLI client: builds control requests, auto-spawns the daemon, prints results.
//!
//! CLI and MCP are Layer-B clients of the daemon (`docs/UI.md`); the web
//! application uses the same contract through its loopback adapter. This module
//! renders `Response` snapshots for a human shell, or the versioned
//! `--json` DTO for scripts/agents. Exit codes: `0` ok · `1`
//! usage/error · `2` ref not found / ambiguous · `3` daemon unreachable.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};

use crate::{
    client_action::{ClientAction, ClientPayload},
    control::{self, request, ControlRoute, ErrorKind, Event, EventKind, Request, Response},
    daemon::{ClientScope, LocalOrbitId, OrbitAddress},
    diagnose::{DiagnosisView, GateState},
    spaces::{self, SpaceEntry, StorePresence},
};

/// Output mode threaded from the global `--json` / `--no-color` / `--yes` flags.
#[derive(Debug, Clone, Copy)]
pub struct Out {
    pub json: bool,
    pub color: bool,
    /// `--yes`: assume yes at every confirmation prompt. See [`confirm`].
    pub yes: bool,
}

impl Default for Out {
    fn default() -> Self {
        Out {
            json: false,
            color: true,
            yes: false,
        }
    }
}

/// A client-side failure that carries its own exit code.
///
/// Daemon-side failures already travel with a typed [`ErrorKind`], and
/// `exit_code_for_kind` derives their code "from the typed kind, not the message
/// text". This extends the same rule to failures that never reach the daemon —
/// the alternative is a top-level reporter pattern-matching prose, which is
/// exactly what that rule exists to prevent. Plain `anyhow` errors stay code `1`,
/// so classifying is opt-in and nothing has to be reclassified at once.
#[derive(Debug)]
pub struct CliError {
    /// The documented exit code for this failure.
    pub code: i32,
    pub message: String,
}

impl CliError {
    /// `2` — a selector resolved to nothing. Matches what the daemon already
    /// returns for a missing ref, user, or label, so a missing *space* doesn't
    /// answer differently to the same kind of mistake.
    pub fn not_found(message: impl Into<String>) -> Self {
        CliError {
            code: 2,
            message: message.into(),
        }
    }

    /// `3` — the daemon could not be reached, or could not be understood.
    pub fn unreachable(message: impl Into<String>) -> Self {
        CliError {
            code: 3,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// The exit code represented by a client-side error. Split from
/// [`report_error`] so the mapping is testable without a process to exit —
/// `ExitCode` is deliberately opaque and can't be read back.
///
/// An unclassified error is `1`, so the classification is additive: a plain
/// `anyhow!` keeps behaving exactly as it did.
fn exit_code_for_error(e: &anyhow::Error) -> i32 {
    if let Some(c) = e.downcast_ref::<CliError>() {
        return c.code;
    }
    // Something is listening and no request will ever get through to it: `3`,
    // daemon unreachable, in the sense that matters.
    if e.downcast_ref::<ForeignDaemon>().is_some() {
        return 3;
    }
    1
}

/// Report a failure and return the process exit code — the one place a
/// client-side error becomes output.
///
/// `main` used to be `async fn main() -> Result<()>`, which handed every such
/// error to anyhow's `Termination` impl. That broke four contracts at once, all
/// of which this fixes:
///
/// * **One voice.** `Error:` (anyhow's `Debug`) and `error:` (the daemon path)
///   both shipped in one binary. Now everything is the lowercase form.
/// * **No internals.** `Debug` prints the `Caused by:` chain, which surfaced raw
///   `data-encoding` and `postcard` text ("non-zero trailing bits at 3") on a
///   truncated invite. `{e:#}` is the single-line `context: cause` form.
/// * **`--json` is a contract.** A consumer got prose on stderr and *nothing* on
///   stdout, unable to tell failure from an empty result.
/// * **Exit codes are typed.** `Termination` exits `1` for everything, so a
///   not-found answered `1` while the documented code is `2`.
pub fn report_error(e: &anyhow::Error, out: Out) -> std::process::ExitCode {
    let code = exit_code_for_error(e);
    // The single-line form: "context: cause", never the multi-line chain.
    let message = format!("{e:#}");
    if out.json {
        // Same DTO shape the daemon path emits, so a script parses one thing.
        let resp = if code == 2 {
            Response::not_found(message)
        } else {
            Response::err(message)
        };
        println!(
            "{}",
            serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into())
        );
    } else {
        eprintln!("error: {message}");
    }
    std::process::ExitCode::from(code as u8)
}

/// What a confirmation prompt decided, and why — so the caller can tell "the
/// user said no" (a clean exit) from "we couldn't ask" (an error that must
/// name the flag that would have worked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmed {
    /// Go ahead: the user typed `y`, or `--yes` was passed.
    Yes,
    /// The user answered, and the answer was no.
    No,
    /// We could not ask — no TTY (CI, a pipe, the MCP server), or `--json`.
    /// Never block in these; the caller must fail and print the `--yes` form.
    CannotAsk,
}

/// Ask a yes/no question, defaulting to **no**.
///
/// The one place lait prompts, so every destructive verb and every repair offer
/// degrades identically:
///
/// * `--yes` → [`Confirmed::Yes`] without asking (scripts, CI, agents).
/// * `--json` or no TTY on **stdin or stdout** → [`Confirmed::CannotAsk`]. A
///   prompt written into a pipe is invisible, and reading a reply from a
///   redirected stdin would eat data meant for the command (`lait issues comment`
///   reads stdin) or block forever with no visible question. Both checks matter:
///   stdout carries the question, stdin carries the answer.
/// * otherwise → ask on **stderr** (stdout is the data channel; a prompt must
///   never land in `lait issues ls | cat`), read one line, `y`/`yes` is yes and
///   everything else — including a bare Enter or EOF — is no.
pub fn confirm(question: &str, out: Out) -> Confirmed {
    if out.yes {
        return Confirmed::Yes;
    }
    use std::io::IsTerminal;
    if out.json || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Confirmed::CannotAsk;
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush().ok();
    let mut reply = String::new();
    if std::io::stdin().read_line(&mut reply).is_err() {
        return Confirmed::No;
    }
    match reply.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Confirmed::Yes,
        _ => Confirmed::No,
    }
}

/// Minimal ANSI styling. Every helper is gated on `Out.color`, which already
/// folds in `--no-color`, `$NO_COLOR`, `--json`, and TTY detection (computed once
/// in `app::run`), so a renderer just passes `out.color` and never re-checks.
mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const DIM: &str = "\x1b[2m";
    pub const BOLD: &str = "\x1b[1m";
    pub const RED: &str = "\x1b[31m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const CYAN: &str = "\x1b[36m";
}

/// Wrap `s` in an ANSI code when `on`, else return it unstyled.
fn paint(on: bool, code: &str, s: &str) -> String {
    if on {
        format!("{code}{s}{}", ansi::RESET)
    } else {
        s.to_string()
    }
}

/// The confirmation question for a request that destroys something, or `None`
/// for the ones that don't.
///
/// Deliberately keyed on the `Request` rather than the command name: this is the
/// single list of what lait asks before doing, so adding a destructive verb means
/// adding it here, not remembering to prompt at a call site.
/// The question a destructive verb must answer before it runs, or `None` if the
/// verb destroys nothing.
///
/// Keyed on the `Request` so the list lives in exactly one place, whatever asks
/// it. The CLI asks on a TTY (`confirm_destructive`); `lait serve` hands the same
/// string to the browser to put in a modal. A second copy of this list, phrased
/// slightly differently, is how two surfaces end up disagreeing about what is
/// dangerous.
pub(crate) fn destructive_question(req: &Request) -> Option<String> {
    match req {
        Request::MemberRemove { who } => Some(format!(
            "remove {who} from this space and rotate the space key?"
        )),
        Request::KeyRotate => Some("rotate the space key?".to_string()),
        _ => None,
    }
}

/// Gate one package-declared destructive operation behind the shell's common
/// confirmation affordance.
pub fn confirm_client(question: &str, out: Out) -> bool {
    ask_confirmation(question, out)
}

/// Gate a destructive request behind a confirmation. `true` = go ahead.
///
/// Non-destructive requests pass straight through, so this can sit on the uniform
/// dispatch path without every verb paying for it.
pub async fn confirm_destructive(req: &Request, out: Out) -> bool {
    let Some(question) = destructive_question(req) else {
        return true;
    };
    ask_confirmation(&question, out)
}

fn ask_confirmation(question: &str, out: Out) -> bool {
    match confirm(question, out) {
        Confirmed::Yes => true,
        Confirmed::No => {
            eprintln!("aborted.");
            false
        }
        Confirmed::CannotAsk => {
            eprintln!(
                "error: {question}\n       \
                 this needs confirmation and there is no terminal to ask on — \
                 re-run with `--yes` to confirm."
            );
            false
        }
    }
}

/// Where a spawned daemon's stderr goes. Truncated per spawn (we only spawn when
/// none is running, so it holds exactly the current daemon's life), and inside
/// `home`, which is `*`-gitignored.
pub fn daemon_log_path(home: &Path) -> std::path::PathBuf {
    home.join("daemon.log")
}

/// The last few lines of the daemon log — a dying daemon's own account of why,
/// which is otherwise thrown away.
fn daemon_log_tail(path: &Path, lines: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let tail: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(lines)
        .collect();
    if tail.is_empty() {
        return None;
    }
    Some(
        tail.into_iter()
            .rev()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Ensure the identity-scoped Lait daemon is running and the addressed Orbit
/// exists, spawning the one host process if needed.
pub async fn ensure_daemon(home: &Path) -> Result<()> {
    if !crate::orbital::space_store_present(home) {
        return Err(anyhow!(
            "no space at {} — found one with `lait init`, or join one with `lait join <link>`",
            home.display()
        ));
    }
    ensure_lait_daemon().await
}

/// Ensure the current identity's process-level Lait daemon without selecting
/// or activating an Orbit. The web viewer uses this even for an empty catalog.
pub async fn ensure_lait_daemon() -> Result<()> {
    let client = crate::daemon::LaitDaemonClient::current()?;
    let daemon_home = client.home();
    match client.probe().await {
        control::Probe::Healthy => return Ok(()),
        control::Probe::Foreign { why, replaceable } => {
            return Err(ForeignDaemon {
                home: daemon_home.to_path_buf(),
                why,
                replaceable,
            }
            .into())
        }
        control::Probe::Absent => {}
    }
    let exe = std::env::current_exe().context("locate own executable")?;
    let log_path = daemon_log_path(daemon_home);
    let log = std::fs::File::create(&log_path).ok();
    // Passing the ordinary config root through `--home` would collapse the
    // global catalog into a self-contained identity, so pin only an explicit
    // self-contained `$LAIT_HOME`.
    let identity = std::env::var_os("LAIT_HOME").map(PathBuf::from);
    let mut child =
        crate::daemon_spawn::spawn(&exe, log, identity.as_deref()).context("spawn Lait daemon")?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if matches!(client.probe().await, control::Probe::Healthy) {
            return Ok(());
        }
        // A daemon that has already exited is never going to answer. Without this
        // the common failures (lock held, bind failure) each cost the full 20s
        // and then blame a timeout.
        if let Ok(Some(status)) = child.try_wait() {
            // But *our* child dying is not the same as no daemon. Two processes
            // can race to spawn one for the same home: the loser exits saying
            // "another lait daemon is already running" while the winner's is up
            // and answering. Losing that race is success — the home has the daemon
            // it needs, it just isn't ours — so ask before blaming. Rare between
            // two CLI invocations; routine once `lait serve` is in the mix, since
            // it holds several homes and reacts to doorbells while you type.
            //
            // Ask for a moment, not once: the winner is starting at the same
            // instant our loser gives up, so a single immediate probe usually
            // arrives too early and blames a daemon that is seconds from
            // answering. A lost race resolves in milliseconds; a genuinely broken
            // spawn (bind failure, held lock) still fails in about a second rather
            // than the full 20 this check exists to avoid.
            for _ in 0..12 {
                if matches!(client.probe().await, control::Probe::Healthy) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            return Err(daemon_exited_error(status, &log_path));
        }
    }
    Err(CliError::unreachable(format!(
        "Lait daemon did not come online within 20s — it is running but not answering.\n\
         see {log}, or run `lait daemon` in the foreground to watch it start.",
        log = log_path.display(),
    ))
    .into())
}

/// Detect a daemon this build can't talk to and offer to clear it.
///
/// The house pattern for a recoverable bad state: **detect** it precisely,
/// **inform** in the user's terms, **offer** the fix, **verify** it worked, and
/// **degrade** without blocking when there's nobody to ask. Informing alone would
/// leave every verb dead until the user hand-runs a command we already know the
/// name of.
///
/// Detection is at the transport level (see [`control::probe`]) because that is
/// the one thing a wire-shape change cannot break — which matters here, since the
/// whole condition *is* a wire-shape change.
/// `true` if `e` is a foreign daemon this build may replace — i.e. worth offering
/// [`heal_from_error`] and retrying.
pub fn is_replaceable_foreign(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ForeignDaemon>()
        .is_some_and(|f| f.replaceable)
}

/// Offer to clear a daemon this build can't talk to. `Ok(())` = repaired; the
/// caller may retry what failed.
///
/// Driven from the **error path**, never a probe up front: the happy path (a
/// healthy daemon, or none) must not pay a connect for a repair it will never
/// need. Errors are the only place this condition exists, so that is where the
/// offer belongs.
pub async fn heal_from_error(e: &anyhow::Error, out: Out) -> Result<()> {
    let Some(f) = e.downcast_ref::<ForeignDaemon>() else {
        return Err(anyhow!("{e:#}"));
    };
    // Only offer the repair when *we* are the newer side. Offering to stop a
    // daemon that is ahead of this build would be offering to break the node:
    // a downgrade at best, and an unopenable store at worst. There, the only
    // honest answer is the one the handshake already gives — upgrade.
    if !f.replaceable {
        return Err(anyhow!("{e:#}"));
    }
    let pid = crate::config::daemon_pid(&f.home)
        .map(|p| format!(" (pid {p})"))
        .unwrap_or_default();
    // `why` comes from the version handshake, so it names the actual mismatch
    // ("speaks control protocol v1, this build speaks v2") rather than whichever
    // field happened to fail to decode.
    eprintln!(
        "the Lait daemon is already running{pid}: {why}",
        why = f.why
    );
    match confirm("stop it and continue?", out) {
        Confirmed::Yes => {
            stop_daemon_verified(&f.home).await?;
            eprintln!("stopped it — continuing.");
            Ok(())
        }
        Confirmed::No => Err(anyhow!(
            "left it running — `lait shutdown` stops it when you're ready"
        )),
        Confirmed::CannotAsk => Err(anyhow!(
            "run `lait shutdown` to stop it, or re-run with `--yes` to stop it \
             automatically"
        )),
    }
}

/// Poll until nothing is listening on this home, or `within` elapses.
/// `true` = the daemon is really gone.
async fn wait_until_absent(home: &Path, within: Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    loop {
        if matches!(control::probe(home).await, control::Probe::Absent) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Stop the daemon for `home` and **verify** it actually stopped.
///
/// Never trusts the acknowledgement. A v0.4.8-era daemon answers `stop` with
/// "shutting down" and then keeps running — its `notify_one` could hand the lone
/// permit to a subscriber instead of the accept loop (fixed in
/// `node::signal_shutdown`, but the daemons that need stopping are precisely the
/// ones that predate the fix). So: ask, watch, and escalate if it lied.
pub async fn stop_daemon_verified(_home: &Path) -> Result<()> {
    let daemon_home = crate::config::lait_daemon_home()?;
    // Read the pid before asking — a daemon that honours `stop` takes its lock
    // file with it, and we'd rather have the signal target than race for it.
    let pid = crate::config::daemon_pid(&daemon_home);
    let daemon = crate::daemon::LaitDaemonClient::at(daemon_home.clone());
    let _ = daemon
        .request(ControlRoute::Daemon, &Request::Stop, None)
        .await;
    if wait_until_absent(&daemon_home, Duration::from_secs(3)).await {
        return Ok(());
    }
    let Some(pid) = pid else {
        return Err(anyhow!(
            "the daemon ignored `stop` and its lock file names no pid (it predates \
             the pid stamp) — find it with `ps aux | grep 'lait daemon'` and kill it"
        ));
    };
    #[cfg(unix)]
    {
        for sig in [libc::SIGTERM, libc::SIGKILL] {
            // SAFETY: kill(2) with a pid read from this home's lock file, sending
            // a standard termination signal. An already-dead pid just returns
            // ESRCH, which the wait below treats as gone.
            unsafe { libc::kill(pid as libc::pid_t, sig) };
            if wait_until_absent(&daemon_home, Duration::from_secs(3)).await {
                return Ok(());
            }
        }
    }
    Err(anyhow!(
        "could not stop the daemon (pid {pid}) — kill it by hand and re-run"
    ))
}

/// A daemon is listening on this home that this build cannot talk to — in
/// practice a version skew (the binary was upgraded, the daemon wasn't restarted).
///
/// Typed rather than a message, so the repair can be offered from the error path
/// (see [`heal_from_error`]) instead of probing eagerly on every command that
/// will never need it. Exit code `3`: unreachable in the sense that matters —
/// something is there, and no request will ever get through to it.
#[derive(Debug)]
pub struct ForeignDaemon {
    pub home: PathBuf,
    /// The handshake's own diagnosis; already carries the way out.
    pub why: String,
    /// Whether replacing it is the right repair — false when it is ahead of us.
    pub replaceable: bool,
}

impl std::fmt::Display for ForeignDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the Lait daemon is already running, but {why}  (home: {home})",
            why = self.why,
            home = self.home.display(),
        )
    }
}

impl std::error::Error for ForeignDaemon {}

/// The spawned daemon exited before answering — report its own last words rather
/// than a timeout.
fn daemon_exited_error(status: std::process::ExitStatus, log_path: &Path) -> anyhow::Error {
    match daemon_log_tail(log_path, 5) {
        Some(tail) => anyhow!(
            "the daemon exited immediately ({status}). it said:\n{tail}\n\
             full log: {log}",
            log = log_path.display(),
        ),
        None => anyhow!(
            "the daemon exited immediately ({status}) without saying why (see {log})",
            log = log_path.display(),
        ),
    }
}

/// Ensure the daemon is up, then send one already-routed action as the primary
/// identity — or, if an acting identity is selected, as that local agent.
pub async fn client_action(home: &Path, action: ClientAction) -> Result<Response> {
    let act_as = std::env::var("LAIT_AS")
        .ok()
        .or_else(|| std::env::var("LAIT_AGENT").ok())
        .filter(|s| !s.is_empty());
    let scope = scope_for_home(home);
    client_action_as_scoped(home, action, &scope, act_as.as_deref()).await
}

/// Ensure the daemon is up, then send one request as the primary identity — or,
/// if `$LAIT_AS` names a local agent, as that agent (the shell-scoped selector,
/// e.g. `LAIT_AS=scout lait issues new "…"`). Architecture B's "act as" on the CLI.
pub async fn client(home: &Path, req: Request) -> Result<Response> {
    client_action(home, ClientAction::from_legacy(req)).await
}

/// Ensure the daemon is up, then send one request acting as `act_as` (a local
/// agent name, or `None` for the primary human identity).
pub async fn client_as(home: &Path, req: Request, act_as: Option<&str>) -> Result<Response> {
    let scope = scope_for_home(home);
    client_action_as_scoped(home, ClientAction::from_legacy(req), &scope, act_as).await
}

/// The single-Orbit scope implied by cwd/`--orbit`/`--home` store resolution.
pub fn scope_for_home(home: &Path) -> ClientScope {
    ClientScope::pinned(LocalOrbitId::for_store(home))
}

/// Resolve the complete typed address for the one Orbit under `home`.
pub fn orbit_address_for_home(home: &Path) -> Result<OrbitAddress> {
    let space = crate::orbital::discover_space_id(home)
        .ok_or_else(|| anyhow!("no local Orbit under {}", home.display()))?;
    Ok(OrbitAddress::for_store(home, space))
}

/// Send through a caller scope derived by a trusted client adapter.
///
/// The allowed set never rides on the wire: authorizing locally chooses an
/// explicit Orbit route, and the receiving SpaceBridge independently validates
/// that it occupies the named Orbit and Space.
pub async fn client_as_scoped(
    home: &Path,
    req: Request,
    scope: &ClientScope,
    act_as: Option<&str>,
) -> Result<Response> {
    client_action_as_scoped(home, ClientAction::from_legacy(req), scope, act_as).await
}

/// Send an action whose terminal owner was fixed by the command registry.
pub async fn client_action_as_scoped(
    home: &Path,
    action: ClientAction,
    scope: &ClientScope,
    act_as: Option<&str>,
) -> Result<Response> {
    let address = orbit_address_for_home(home)?;
    scope.authorize(&address)?;
    ensure_daemon(home).await?;
    let daemon = crate::daemon::LaitDaemonClient::current()?;
    // The process endpoint answered the probe a moment ago, so a failure here
    // is the transport giving out mid-exchange: `3`, daemon unreachable.
    match action.payload() {
        ClientPayload::Control(request) => {
            let route = action.route(address);
            daemon
                .request(route, request, act_as)
                .await
                .map_err(|e| CliError::unreachable(format!("{e:#}")).into())
        }
        ClientPayload::World(_) => Err(anyhow!(
            "World actions must be dispatched through their client package"
        )),
    }
}

/// Generic facilities supplied by the Lait navigation shell to one client
/// package invocation.
pub struct PackageClientHost {
    home: PathBuf,
    scope: ClientScope,
    act_as: Option<String>,
}

impl PackageClientHost {
    pub fn new(home: impl Into<PathBuf>, scope: ClientScope, act_as: Option<String>) -> Self {
        Self {
            home: home.into(),
            scope,
            act_as,
        }
    }
}

impl world_interface::ClientHost for PackageClientHost {
    fn local_root(&self) -> &Path {
        &self.home
    }

    fn call_world<'a>(
        &'a self,
        call: crate::orbital::WorldCall,
    ) -> world_interface::ClientFuture<'a, crate::orbital::WorldReply> {
        Box::pin(async move {
            world_reply_as_scoped(&self.home, call, &self.scope, self.act_as.as_deref())
                .await
                .map_err(|error| world_interface::InterfaceError::new(format!("{error:#}")))
        })
    }

    fn call_control<'a>(
        &'a self,
        request: world_interface::HostControlRequest,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let request = match request {
                world_interface::HostControlRequest::AssignmentList { actor } => {
                    Request::AssignmentList { actor }
                }
                world_interface::HostControlRequest::AssignmentGrant { actor, assignments } => {
                    Request::AssignmentGrant {
                        actor,
                        assignments: assignments
                            .into_iter()
                            .map(|assignment| crate::control::AssignmentSpec {
                                world: assignment.world,
                                capability: assignment.capability,
                                resource: assignment.resource,
                            })
                            .collect(),
                    }
                }
                world_interface::HostControlRequest::AssignmentRevoke { grant_id } => {
                    Request::AssignmentRevoke { grant_id }
                }
                world_interface::HostControlRequest::WorldActivate { world } => {
                    Request::WorldActivate {
                        world: world.as_str().to_string(),
                    }
                }
            };
            let response =
                client_as_scoped(&self.home, request, &self.scope, self.act_as.as_deref())
                    .await
                    .map_err(|error| world_interface::InterfaceError::new(format!("{error:#}")))?;
            serde_json::to_value(response).map_err(|error| {
                world_interface::InterfaceError::new(format!(
                    "encode host control response: {error}"
                ))
            })
        })
    }

    fn call_content<'a>(
        &'a self,
        request: world_interface::HostContentRequest,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let fail =
                |error: anyhow::Error| world_interface::InterfaceError::new(format!("{error:#}"));
            let address = orbit_address_for_home(&self.home).map_err(&fail)?;
            self.scope
                .authorize(&address)
                .map_err(|error| fail(anyhow!("{error:#}")))?;
            ensure_daemon(&self.home).await.map_err(&fail)?;
            let daemon = crate::daemon::LaitDaemonClient::current().map_err(&fail)?;
            let route = crate::control::station_route(address);
            match request {
                world_interface::HostContentRequest::Write { path } => {
                    content_write(daemon.home(), route, &path).await
                }
                world_interface::HostContentRequest::Read {
                    content,
                    destination,
                } => content_read(daemon.home(), route, &content, &destination).await,
                world_interface::HostContentRequest::Stat { content } => {
                    content_stat(daemon.home(), route, &content).await
                }
            }
        })
    }
}

/// Stream a local file onto the content plane.
///
/// Read in pieces and forwarded as they arrive: a file larger than memory is
/// the case this whole plane exists for, so materialising it here would defeat
/// the purpose one layer above where it matters.
async fn content_write(
    home: &Path,
    route: crate::control::ControlRoute,
    path: &Path,
) -> Result<serde_json::Value, world_interface::InterfaceError> {
    use tokio::io::AsyncReadExt;

    let fail = |message: String| world_interface::InterfaceError::new(message);
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| fail(format!("could not read {}: {e}", path.display())))?;
    let declared = file
        .metadata()
        .await
        .map_err(|e| fail(format!("could not measure {}: {e}", path.display())))?
        .len();
    if declared == 0 {
        return Err(fail(format!(
            "{} is empty — nothing to attach",
            path.display()
        )));
    }
    let mut operation = [0u8; 16];
    getrandom::fill(&mut operation).map_err(|e| fail(format!("operation id: {e}")))?;
    let mut upload = crate::control::ContentUpload::open(home, route, operation, None, declared)
        .await
        .map_err(|e| fail(format!("{e:#}")))?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| fail(format!("could not read {}: {e}", path.display())))?;
        if read == 0 {
            break;
        }
        upload
            .push(&buffer[..read])
            .await
            .map_err(|e| fail(format!("{e:#}")))?;
    }
    match upload.finish().await.map_err(|e| fail(format!("{e:#}")))? {
        crate::control::ContentReply::ContentWritten {
            content,
            plaintext_len,
        } => Ok(serde_json::json!({ "content": content, "size": plaintext_len })),
        crate::control::ContentReply::ContentError { message, .. } => Err(fail(message)),
        other => Err(fail(format!("unexpected answer: {other:?}"))),
    }
}

/// Stream a committed content to a local path.
///
/// Written to a temporary beside the destination and renamed, so an interrupted
/// save leaves either the old file or none — never a half-written one under the
/// name somebody will open.
async fn content_read(
    home: &Path,
    route: crate::control::ControlRoute,
    content: &str,
    destination: &Path,
) -> Result<serde_json::Value, world_interface::InterfaceError> {
    use tokio::io::AsyncWriteExt;

    let fail = |message: String| world_interface::InterfaceError::new(message);
    let temporary = destination.with_extension("lait-partial");
    let mut file = tokio::fs::File::create(&temporary)
        .await
        .map_err(|e| fail(format!("could not write {}: {e}", temporary.display())))?;
    let mut offset = 0u64;
    loop {
        let (reply, bytes) = crate::control::content_call(
            home,
            &crate::control::content_request(
                route.clone(),
                crate::control::ContentCall::Read {
                    content: content.to_string(),
                    offset,
                    len: runtime::content_host::MAX_RANGE_BYTES as u64,
                },
            ),
        )
        .await
        .map_err(|e| fail(format!("{e:#}")))?;
        match reply {
            crate::control::ContentReply::ContentStream { .. } => {}
            crate::control::ContentReply::ContentError { message, .. } => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(fail(message));
            }
            other => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(fail(format!("unexpected answer: {other:?}")));
            }
        }
        if bytes.is_empty() {
            break;
        }
        file.write_all(&bytes)
            .await
            .map_err(|e| fail(format!("could not write {}: {e}", temporary.display())))?;
        offset += bytes.len() as u64;
    }
    file.flush().await.ok();
    // Durable before it is visible: the rename is what publishes the file, and
    // renaming an unflushed file publishes a name over bytes that may not be
    // there after a crash.
    file.sync_all()
        .await
        .map_err(|e| fail(format!("could not flush {}: {e}", temporary.display())))?;
    drop(file);
    tokio::fs::rename(&temporary, destination)
        .await
        .map_err(|e| fail(format!("could not write {}: {e}", destination.display())))?;
    Ok(serde_json::json!({ "size": offset }))
}

async fn content_stat(
    home: &Path,
    route: crate::control::ControlRoute,
    content: &str,
) -> Result<serde_json::Value, world_interface::InterfaceError> {
    let fail = |message: String| world_interface::InterfaceError::new(message);
    let (reply, _) = crate::control::content_call(
        home,
        &crate::control::content_request(
            route,
            crate::control::ContentCall::Stat {
                content: content.to_string(),
            },
        ),
    )
    .await
    .map_err(|e| fail(format!("{e:#}")))?;
    match reply {
        crate::control::ContentReply::ContentStatus {
            content,
            plaintext_len,
            chunk_count,
            resident_chunks,
            pinned,
        } => Ok(serde_json::json!({
            "content": content,
            "size": plaintext_len,
            "chunk_count": chunk_count,
            "resident_chunks": resident_chunks,
            "pinned": pinned,
        })),
        crate::control::ContentReply::ContentError { message, .. } => Err(fail(message)),
        other => Err(fail(format!("unexpected answer: {other:?}"))),
    }
}

/// Emit a complete product-owned presentation without inspecting its response.
pub fn print_presentation(presentation: &world_interface::Presentation) -> i32 {
    print!("{}", presentation.stdout);
    eprint!("{}", presentation.stderr);
    presentation.exit_code
}

/// Send one package-owned call without decoding its opaque reply in the shell.
pub async fn world_reply_as_scoped(
    home: &Path,
    call: crate::orbital::WorldCall,
    scope: &ClientScope,
    act_as: Option<&str>,
) -> Result<crate::orbital::WorldReply> {
    let address = orbit_address_for_home(home)?;
    scope.authorize(&address)?;
    let route = ControlRoute::World {
        address,
        world: call.world().as_str().to_string(),
    };
    ensure_daemon(home).await?;
    crate::daemon::LaitDaemonClient::current()?
        .call_world(route, call, act_as)
        .await
        .map_err(|error| CliError::unreachable(format!("{error:#}")).into())
}

/// Send through the current Lait daemon only if it is already running.
///
/// This preserves best-effort surfaces such as live config reload and the
/// optional actor line in `lait id`: they must not start a background service
/// merely to deliver an advisory request.
pub async fn request_running(home: &Path, req: &Request, act_as: Option<&str>) -> Result<Response> {
    if act_as.is_some() {
        return Err(anyhow!("passive requests do not select an acting identity"));
    }
    let address = orbit_address_for_home(home)?;
    let scope = scope_for_home(home);
    scope.authorize(&address)?;
    let action = ClientAction::from_legacy(req.clone());
    let route = action.route(address);
    let daemon = crate::daemon::LaitDaemonClient::current()?;
    if !matches!(daemon.probe().await, control::Probe::Healthy) {
        return Err(anyhow!("Lait daemon is not running"));
    }
    daemon.request_if_running(route, req).await
}

/// Run a request, print the response, and exit with the corresponding code.
pub async fn run(home: &Path, req: Request, out: Out) -> Result<()> {
    run_action(home, ClientAction::from_legacy(req), out).await
}

/// Run an already-routed action, print the response, and preserve CLI exits.
pub async fn run_action(home: &Path, action: ClientAction, out: Out) -> Result<()> {
    match client_action(home, action).await {
        Ok(resp) => {
            let code = print_response(&resp, out);
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        // Propagate rather than reporting here: `client` errors are already
        // classified (`CliError::unreachable`), and the top-level reporter is what
        // honours `--json`. This arm used to print and `exit(3)` itself, which
        // hardcoded "daemon unreachable" onto conditions that weren't — including
        // `ensure_daemon`'s "no space at …", a missing store.
        Err(e) => Err(e),
    }
}

/// Emit a bare text value while honoring the `--json` contract: the
/// `Response::Text` DTO under `--json`, else the raw string. For client-side
/// commands (`id`, `invite`) that don't round-trip a daemon `Response` but must
/// still emit a parseable DTO under `--json` instead of leaking plain text.
pub fn emit_text(text: &str, out: Out) {
    if out.json {
        let resp = Response::Text {
            text: text.to_string(),
        };
        println!(
            "{}",
            serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!("{text}");
    }
}

/// Emit an acknowledgement honouring `--json`: the `Response::Ok` DTO under
/// `--json`, else the human message (`init`, `install-mcp`, `resume`).
pub fn emit_ok(message: &str, out: Out) {
    if out.json {
        let resp = Response::Ok {
            message: Some(message.to_string()),
        };
        println!(
            "{}",
            serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!("{message}");
    }
}

/// Render the guided-join verifier's gate list (human output). Each gate is a
/// coloured glyph + label + detail, followed by the one-line summary keyed off the
/// blocking gate. Under `--json` the caller emits the DTO instead (handled in
/// `print_response`), so this is the human path only.
fn print_diagnosis(v: &DiagnosisView, out: Out) {
    for g in &v.gates {
        let code = match g.state {
            GateState::Pass => ansi::GREEN,
            GateState::Wait => ansi::YELLOW,
            GateState::Warn => ansi::YELLOW,
            GateState::Fail => ansi::RED,
            GateState::Skip => ansi::DIM,
        };
        let glyph = paint(out.color, code, g.state.glyph());
        println!("{} {:<11} {}", glyph, g.label, g.detail);
    }
    println!();
    let code = if v.blocked_on.is_some() {
        ansi::YELLOW
    } else {
        ansi::GREEN
    };
    println!("{}", paint(out.color, code, &v.summary));
}

/// `join` display: send the join, echo the daemon's ack, then run the guided-join
/// verifier as a tail — passing the ticket's space as `expected_space`, so
/// a directory/store mismatch (the joiner ran `join` in the wrong folder) is caught
/// and named immediately instead of surfacing later as a blank board. Under
/// `--json` we emit only the join DTO (no verifier chrome), mirroring `run_invite`.
pub async fn run_join(home: &Path, ticket: String, out: Out) -> Result<()> {
    // Parse client-side to recover the intended space before the link is
    // moved into the request. A malformed link simply yields no expectation;
    // the daemon returns the real parse error.
    let parsed = runtime::SignedCoordinates::parse_link(ticket.trim())
        .ok()
        .and_then(|c| c.verify().ok());
    // An admission-carrying link admits automatically within seconds, so a
    // pending membership is worth polling out.
    let has_pass = parsed.as_ref().is_some_and(|v| v.admission.is_some());
    let expected = parsed.map(|v| v.space.as_str().to_string());
    let resp = client(home, Request::Join { ticket }).await?;
    match &resp {
        Response::Ok { message } => {
            if out.json {
                emit_ok(message.as_deref().unwrap_or("ok"), out);
                return Ok(());
            }
            println!("{}", message.as_deref().unwrap_or("ok"));
        }
        // A join error (bad ticket, unreachable host) is terminal — print and stop.
        other => {
            let code = print_response(other, out);
            if code != 0 {
                std::process::exit(code);
            }
            return Ok(());
        }
    }
    // Human tail: the gate readout. Best-effort — a verifier hiccup must not make a
    // successful join look failed, so we degrade to a hint rather than erroring.
    //
    // Polled, not one-shot: right after `join` returns, admission (Pattern A's
    // auto-seal) and the gossip handshake are still in flight, so a t=0 snapshot
    // reads "waiting on a peer" moments before everything passes — the verifier
    // itself becoming the unreliable reporter. We re-diagnose until the gates
    // settle (all pass, or a Fail-state blocker that time won't clear) or a
    // deadline, and report the settled truth.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last: Option<Response> = None;
    loop {
        match client(
            home,
            Request::Diagnose {
                expected_space: expected.clone(),
            },
        )
        .await
        {
            Ok(diag) => {
                let settled = match &diag {
                    Response::Diagnosis(v) => match v.blocked_on.as_deref() {
                        None => true,
                        // `space` is the one Fail-state blocker (wrong
                        // directory/store) — waiting can't clear it.
                        Some("space") => true,
                        // Pending membership clears itself only under a pass
                        // (Pattern A auto-seal); pass-less waits on a human.
                        Some("membership") => !has_pass,
                        // peer / synced — convergence in flight; keep polling.
                        Some(_) => false,
                    },
                    // Not a diagnosis (daemon error) — nothing to wait out.
                    _ => true,
                };
                let expired = tokio::time::Instant::now() >= deadline;
                if settled || expired {
                    print_diagnosis_or(&diag, out);
                    break;
                }
                last = Some(diag);
            }
            Err(e) => {
                // Degrade to the freshest readout we have, or a hint.
                match &last {
                    Some(diag) => print_diagnosis_or(diag, out),
                    None => eprintln!("(joined; run `lait doctor` for status — {e:#})"),
                }
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

/// Render a `Diagnosis` response, or fall back gracefully if the daemon returned
/// some other variant (e.g. an error) to the tail request.
fn print_diagnosis_or(resp: &Response, out: Out) {
    match resp {
        Response::Diagnosis(v) => print_diagnosis(v, out),
        other => {
            print_response(other, out);
        }
    }
}

/// Print the current navigation context without activating an Orbit.
pub async fn print_context(home: Option<PathBuf>, source: &str, out: Out) -> Result<()> {
    let selected = home
        .filter(|path| crate::orbital::space_store_present(path))
        .and_then(|path| {
            let space = crate::orbital::discover_space_id(&path)?;
            Some((
                LocalOrbitId::for_store(&path).to_string(),
                space.to_string(),
                path,
            ))
        });
    let worlds: Vec<String> = crate::world::packages()
        .world_ids()
        .map(ToString::to_string)
        .collect();
    let acting_as = std::env::var("LAIT_AS")
        .ok()
        .or_else(|| std::env::var("LAIT_AGENT").ok())
        .filter(|value| !value.is_empty());
    let identity_home = crate::config::identity_dir()?;

    if out.json {
        let orbit = selected.as_ref().map(|(orbit, space, path)| {
            serde_json::json!({
                "id": orbit,
                "space": space,
                "path": path,
            })
        });
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "context": {
                    "selection": source,
                    "identity": {
                        "home": identity_home,
                        "acting_as": acting_as,
                    },
                    "orbit": orbit,
                    "worlds": worlds,
                    "known_orbits": spaces::list().len(),
                }
            }))
            .unwrap_or_else(|_| "{}".into())
        );
        return Ok(());
    }

    println!("{}", paint(out.color, ansi::BOLD, "lait context"));
    println!("identity   {}", acting_as.as_deref().unwrap_or("primary"));
    println!("selection  {source}");
    if let Some((orbit, space, path)) = selected {
        println!("orbit      {orbit}");
        println!("space      {space}");
        println!("path       {}", path.display());
    } else {
        println!("orbit      (none selected)");
        println!("space      (none selected)");
    }
    println!(
        "worlds     {}",
        if worlds.is_empty() {
            "(none installed)".to_string()
        } else {
            worlds.join(", ")
        }
    );
    if source == "none" {
        let known = spaces::list().len();
        if known == 0 {
            println!();
            println!("found a Space with `lait init`, or enter one with `lait join <link>`.");
        } else {
            println!();
            println!(
                "{known} local Orbit{} known — select one with `lait --orbit <selector>`.",
                if known == 1 { "" } else { "s" }
            );
        }
    }
    Ok(())
}

/// List World packages installed in this application composition.
pub fn print_worlds(out: Out) {
    let worlds: Vec<String> = crate::world::packages()
        .world_ids()
        .map(ToString::to_string)
        .collect();
    if out.json {
        let rows: Vec<_> = worlds
            .iter()
            .map(|world| serde_json::json!({ "id": world, "installed": true }))
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({ "worlds": rows }))
                .unwrap_or_else(|_| "{}".into())
        );
    } else if worlds.is_empty() {
        println!("(no World packages installed)");
    } else {
        for world in worlds {
            println!("{world}");
        }
    }
}

/// Live status of one registry entry: `missing` (store gone from disk), `up`
/// (a daemon answers on its control channel), or `idle` (store present, no
/// daemon). The probe is a short-deadline `Status` round-trip — never a spawn.
async fn orbit_status(e: &SpaceEntry) -> &'static str {
    if spaces::presence(e) == StorePresence::Missing {
        return "missing";
    }
    let up = tokio::time::timeout(
        Duration::from_millis(300),
        request(Path::new(&e.path), &Request::Status),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);
    if up {
        "up"
    } else {
        "idle"
    }
}

/// `lait orbits`: every durable local participation known on this machine.
pub async fn print_orbits(out: Out) {
    let entries = spaces::list();
    let mut statuses = Vec::with_capacity(entries.len());
    for e in &entries {
        statuses.push(orbit_status(e).await);
    }
    if out.json {
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .zip(&statuses)
            .map(|(e, s)| {
                let mut v = serde_json::to_value(e).unwrap_or_default();
                if let Some(o) = v.as_object_mut() {
                    o.insert(
                        "orbit".into(),
                        serde_json::json!(LocalOrbitId::for_store(Path::new(&e.path))),
                    );
                    o.insert("status".into(), serde_json::json!(s));
                }
                v
            })
            .collect();
        println!(
            "{}",
            // `spaces` is retained as a compatibility key for scripts written
            // against the former command name. Its rows were always path-keyed
            // local participations; `orbits` names that truth.
            serde_json::to_string(&serde_json::json!({
                "orbits": rows.clone(),
                "spaces": rows,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
        return;
    }
    if entries.is_empty() {
        println!("(no Orbits yet — `lait init` to found one, or `lait join <link>`)");
        return;
    }
    for (e, status) in entries.iter().zip(&statuses) {
        let orbit = LocalOrbitId::for_store(Path::new(&e.path));
        let orbit_short: String = orbit.as_str().chars().take(14).collect();
        let space_short: String = e.space.chars().take(12).collect();
        let code = match *status {
            "up" => ansi::GREEN,
            "idle" => ansi::DIM,
            _ => ansi::RED,
        };
        let name = if e.name.is_empty() {
            "(unnamed)"
        } else {
            &e.name
        };
        let projects = if e.projects.is_empty() {
            String::new()
        } else {
            let keys: Vec<&str> = e.projects.iter().map(|p| p.key.as_str()).collect();
            format!("  [{}]", keys.join(", "))
        };
        let nick = if e.host_nick.is_empty() {
            String::new()
        } else {
            format!("  (from {})", e.host_nick)
        };
        println!(
            "{name}  {orbit_short}  {space_short}  {}  {}{projects}{nick}",
            e.origin,
            paint(out.color, code, status),
        );
        println!("  {}", paint(out.color, ansi::DIM, &e.path));
    }
}

/// The universal "no space here" error: any store-needing command run in a
/// directory with no discoverable `.lait/` gets this instead of a silently
/// minted decoy store. Points at the creation verbs and every known space.
pub fn err_no_store_here(out: Out) {
    eprintln!("no lait space in this directory (nothing is created implicitly).");
    let known = spaces::list();
    if !known.is_empty() {
        eprintln!();
        eprintln!("local Orbits on this machine:");
        for e in &known {
            let name = if e.name.is_empty() {
                "(unnamed)"
            } else {
                &e.name
            };
            eprintln!(
                "  {} {name}  \u{2192}  {}",
                paint(out.color, ansi::DIM, "\u{2022}"),
                e.path
            );
        }
        eprintln!();
        eprintln!(
            "cd into one, target one with `--orbit <name>`, or run `lait orbits` for details."
        );
    } else {
        eprintln!();
        eprintln!("found a space here with `lait init`, or join one with `lait join <link>`.");
    }
}

pub fn print_response(resp: &Response, out: Out) -> i32 {
    if out.json {
        let json = serde_json::to_string(resp).unwrap_or_else(|_| "{}".into());
        println!("{json}");
        return match resp {
            Response::Error { error_kind, .. } => exit_code_for_kind(*error_kind),
            _ => 0,
        };
    }
    match resp {
        // Not a user-facing outcome: the handshake belongs to `control::probe`,
        // which reads it as raw JSON before anything is typed. Rendered plainly
        // rather than `unreachable!()` — a panic here would turn a diagnostic
        // into a crash on exactly the mismatched-daemon path this exists for.
        Response::Hello { protocol_version } => {
            println!("control protocol v{protocol_version}");
            0
        }
        Response::Ok { message } => {
            println!("{}", message.as_deref().unwrap_or("ok"));
            0
        }
        Response::Ref { reff } => {
            println!("{reff}");
            0
        }
        Response::Assignments { rows } => {
            if rows.is_empty() {
                println!("(no effective assignments)");
            }
            for r in rows.iter() {
                let scope = if r.resource.is_empty() {
                    "space".to_string()
                } else {
                    r.resource.join("/")
                };
                println!(
                    "{}  {:<24} {:<28} {}",
                    &r.grant_id[..12.min(r.grant_id.len())],
                    r.capability,
                    scope,
                    r.actor
                );
            }
            0
        }
        Response::Members { members } => {
            if members.is_empty() {
                println!("(no members)");
            }
            for m in members {
                let you = if m.me { "  (you)" } else { "" };
                let name = if m.alias.is_empty() {
                    String::new()
                } else {
                    format!("  {}", m.alias)
                };
                // Agents render their sponsor so the delegation chain is visible.
                let sponsor = m
                    .sponsor
                    .as_deref()
                    .map(|s| format!("  via {}", s.chars().take(8).collect::<String>()))
                    .unwrap_or_default();
                let short: String = m.key.chars().take(12).collect();
                println!("{:<7} {}{}{}{}", m.role, short, name, sponsor, you);
            }
            0
        }
        Response::MemberLog { entries } => {
            if entries.is_empty() {
                println!("(no membership ops yet)");
            }
            for e in entries {
                let mark = if e.authorized {
                    paint(out.color, ansi::GREEN, "\u{2713}")
                } else {
                    paint(out.color, ansi::YELLOW, "\u{2717}")
                };
                let actor: String = e.actor.chars().take(8).collect();
                let subject = e
                    .subject
                    .as_deref()
                    .map(|s| s.chars().take(8).collect::<String>())
                    .unwrap_or_default();
                let role = e
                    .role
                    .as_deref()
                    .map(|r| format!(" {r}"))
                    .unwrap_or_default();
                println!("{mark} {actor}  {:<13} {subject}{role}", e.kind);
            }
            0
        }
        Response::Seeds { seeds } => {
            if seeds.is_empty() {
                println!("(no pinned remotes — add one: `lait remote add <ticket>`)");
            }
            for s in seeds {
                let nick = if s.nick.is_empty() { "remote" } else { &s.nick };
                let short: String = s.id.chars().take(12).collect();
                println!("{}  {:<12}  {}", short, nick, s.state);
            }
            0
        }
        Response::Status(s) => {
            println!("id:        {}", s.id);
            println!("nick:      {}", s.nick);
            let ws_line = match (s.name.is_empty(), s.space.as_deref()) {
                (false, Some(ws)) => format!("{} ({ws})", s.name),
                (true, Some(ws)) => ws.to_string(),
                (false, None) => s.name.clone(),
                (true, None) => "(none)".to_string(),
            };
            println!("space:     {ws_line}");
            if !s.membership.is_empty() {
                let code = if s.membership == "pending" {
                    ansi::YELLOW
                } else {
                    ansi::GREEN
                };
                println!("you:       {}", paint(out.color, code, &s.membership));
            }
            if s.counts_unavailable {
                // Never render an unavailable projection as an empty space.
                println!("issues:    (unavailable)");
                println!("projects:  (unavailable)");
            } else {
                println!("issues:    {}", s.issues);
                println!("projects:  {}", s.projects);
            }
            println!("online:    {} peer(s)", s.online_peers);
            // Directional nudges so neither side of a join stalls silently.
            if s.membership == "pending" {
                println!();
                println!(
                    "{}",
                    paint(
                        out.color,
                        ansi::CYAN,
                        "⌛ admission in progress — it completes automatically on the next contact with a member."
                    )
                );
                println!("   the board stays encrypted until then; it syncs automatically once you're in.");
            }
            // A degraded recovery holder is reported on every status, not only
            // when break-glass is attempted: by then it is too late to fix.
            for h in &s.degraded_recovery {
                let why = match &h.reason {
                    mechanics::ceremony::RecoveryArtifactFailure::Undecryptable(_) => {
                        "it was protected under another Windows account or machine"
                    }
                    mechanics::ceremony::RecoveryArtifactFailure::Io(_) => {
                        "it is present but could not be read"
                    }
                };
                let scope = match h.is_current_authority {
                    Some(true) => "the space recovery key",
                    _ => "a recovery key (group unidentified)",
                };
                println!();
                println!(
                    "{}",
                    paint(
                        out.color,
                        ansi::YELLOW,
                        &format!("⚠ your share of {scope} is unusable — {why}.")
                    )
                );
                println!("   transcript: {}", h.transcript);
                println!("   you cannot take part in recovery from this device; other threshold holders still can.");
            }
            0
        }
        Response::Diagnosis(v) => {
            print_diagnosis(v, out);
            0
        }
        Response::Text { text } => {
            println!("{text}");
            0
        }
        Response::Events { events, .. } => {
            if events.is_empty() {
                println!("(no new events)");
            }
            for e in events {
                print_event(e);
            }
            0
        }
        Response::Who { peers } => {
            let mut peers = peers.clone();
            if peers.is_empty() {
                println!("(no peers seen yet)");
            }
            peers.sort_by_key(|p| (!p.online, p.nick.clone()));
            for p in peers {
                let (glyph, code) = match p.state.as_str() {
                    "online" => ("\u{25CF}", ansi::GREEN),
                    "away" => ("\u{25D0}", ansi::YELLOW),
                    _ => ("\u{25CB}", ansi::DIM),
                };
                println!("{} {}  ({})", paint(out.color, code, glyph), p.nick, p.id);
            }
            0
        }
        Response::Live {
            generation,
            partial,
            entries,
        } => {
            if entries.is_empty() {
                println!("(nobody is doing anything here right now)");
            }
            for entry in entries {
                let uncertain = if entry.uncertain { " (uncertain)" } else { "" };
                println!(
                    "{}  {}  {}ms{uncertain}",
                    entry.actor, entry.kind, entry.age_ms
                );
            }
            println!("generation {generation}");
            if *partial {
                // Loud, because an incomplete awareness surface that says
                // nothing is a confident lie about who is here.
                eprintln!("this node is not hearing from everyone it could be");
            }
            0
        }
        Response::LiveUnchanged { generation } => {
            println!("unchanged at generation {generation}");
            0
        }
        Response::Signals { signals, dropped } => {
            if signals.is_empty() {
                println!("(no signals)");
            }
            for entry in signals {
                println!("{}  {:?}", entry.actor, entry.signal);
            }
            if *dropped > 0 {
                eprintln!("{dropped} signal(s) were dropped for want of room");
            }
            0
        }
        Response::Whoami(w) => {
            let none = "—".to_string();
            println!(
                "actor    {}",
                w.actor.as_deref().unwrap_or("(not admitted yet)")
            );
            if let Some(did) = &w.did {
                println!("did      {did}");
            }
            println!("device   {}", w.device);
            println!("space    {}", w.space.as_deref().unwrap_or(&none));
            println!("name     {}", w.name.as_deref().unwrap_or(&none));
            let write = if w.can_write { "write" } else { "view-only" };
            println!("standing {} ({})", w.role, write);
            if let Some(s) = &w.sponsor {
                println!("sponsor  {s}  (sponsored — standing dies with this member)");
            }
            if w.policy_admin {
                println!("policy   policy-admin (can invite + manage policy)");
            }
            if !w.capabilities.is_empty() {
                println!("caps     {}", w.capabilities.join(", "));
            }
            if w.partial_view {
                eprintln!(
                    "{}  view is PARTIAL — run `lait sync`:",
                    paint(out.color, ansi::YELLOW, "!")
                );
                for d in &w.divergence {
                    eprintln!("    - {d}");
                }
            } else {
                println!("view     complete");
            }
            0
        }
        Response::Sync {
            whole,
            divergence,
            message,
        } => {
            if *whole {
                println!("{message}");
                0
            } else {
                eprintln!("{message}");
                for d in divergence {
                    eprintln!("    - {d}");
                }
                1
            }
        }
        Response::Error {
            message,
            error_kind,
        } => {
            eprintln!("error: {message}");
            exit_code_for_kind(*error_kind)
        }
    }
}

/// Exit code from the typed error kind, not from the message text.
fn exit_code_for_kind(kind: ErrorKind) -> i32 {
    match kind {
        ErrorKind::NotFound => 2,
        ErrorKind::Error | ErrorKind::Denied => 1,
    }
}

/// `invite` display: bare token + link + a scannable terminal QR of the link,
/// best-effort clipboard, and the optional `--email <addr>` (open the OS mail
/// client with a prefilled invite). The QR always renders in human output; it is
/// suppressed only under `--json` so scripts get clean, parseable output.
pub async fn run_invite(
    home: &Path,
    email: Option<String>,
    role: Option<String>,
    reusable: bool,
    ttl_hours: Option<u64>,
    out: Out,
) -> Result<()> {
    let resp = client(
        home,
        Request::Invite {
            role,
            reusable,
            ttl_hours,
        },
    )
    .await?;
    let token = match resp {
        Response::Ref { reff } => reff.trim().to_string(),
        other => {
            print_response(&other, out);
            return Ok(());
        }
    };
    // Under --json, emit the ticket as the versioned DTO and stop — no bare
    // lines, no QR/clipboard/mail chrome (the link is derivable from the ticket).
    if out.json {
        emit_text(&token, out);
        return Ok(());
    }
    let link = format!("lait://join/{token}");
    println!("{token}");
    println!("{link}");
    let copied = copy_to_clipboard(&token);
    // The QR is a scan-on-your-phone convenience; an invite ticket is long, so the
    // matrix can be taller/wider than the terminal. Render it only when it fits —
    // otherwise it explodes the scrollback for no gain (the link is right above and
    // on the clipboard). Suppress with $LAIT_NO_QR for a clean, QR-free invite.
    if std::env::var_os("LAIT_NO_QR").is_none() {
        match render_qr(&link) {
            Ok(q) => {
                let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                let qw = q.lines().map(|l| l.chars().count()).max().unwrap_or(0);
                let qh = q.lines().count();
                if qw <= cols as usize && qh + 3 <= rows as usize {
                    println!("\n{q}");
                } else {
                    println!("(QR omitted — too large for this terminal; use the link above)");
                }
            }
            Err(e) => eprintln!("(qr unavailable: {e:#})"),
        }
    }
    if copied {
        println!("(copied to clipboard)");
    }
    // Tell the host what this link actually does, so the mental model matches
    // the flow: accepting the invite IS the approval.
    let hint = if reusable {
        "anyone who runs `lait join <link>` is admitted automatically until it expires"
    } else {
        "your teammate runs `lait join <link>` and is admitted automatically — no approve step"
    };
    println!("→ {hint}");
    if let Some(addr) = email {
        match open_mail_invite(&addr, &link) {
            Ok(()) => {
                if !out.json {
                    println!("(opening your mail client to {addr}…)");
                }
            }
            Err(e) => eprintln!("(could not open mail client: {e:#})"),
        }
    }
    Ok(())
}

/// Copy `s` to the system clipboard, best-effort, using the platform's native
/// tool: `clip` (Windows), `pbcopy` (macOS), or `wl-copy`/`xclip` (Linux).
/// `pub(crate)` so the interactive members picker can copy a fresh invite link.
pub(crate) fn copy_to_clipboard(s: &str) -> bool {
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[
        ("clip", &[]),
        (
            "powershell",
            &["-NoProfile", "-Command", "$input | Set-Clipboard"],
        ),
    ];
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[(&str, &[&str])] =
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])];

    for (cmd, args) in candidates {
        let Ok(mut child) = std::process::Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(s.as_bytes());
        }
        if child.wait().map(|st| st.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Render a scannable QR of the invite link as terminal half-block glyphs. Uses
/// the lowest error-correction level (`L`) so a long invite ticket yields the
/// smallest module count — the QR still scans, but takes far fewer lines than the
/// default level. `pub(crate)` so other local presentation code can reuse it.
pub(crate) fn render_qr(data: &str) -> Result<String> {
    use qrcode::{render::unicode, EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L)
        .context("build QR code")?;
    Ok(code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build())
}

/// Open the OS default mail client with a prefilled invite (mailto). lait sends
/// nothing itself — it just hands the URL to the platform handler.
fn open_mail_invite(addr: &str, link: &str) -> Result<()> {
    let subject = "Invitation to my lait space";
    let body = format!(
        "You're invited to my lait space.\n\n\
         1. Install lait\n   \
         macOS/Linux:  curl --proto '=https' --tlsv1.2 -LsSf \
         https://github.com/nixiesoftware/lait/releases/latest/download/lait-installer.sh | sh\n   \
         Windows:      powershell -c \"irm \
         https://github.com/nixiesoftware/lait/releases/latest/download/lait-installer.ps1 | iex\"\n\n\
         2. Join the space\n   lait join {link}\n\n\
         The link carries a one-time pass, so that admits you automatically and \
         your device gets the space key (run `lait status` to see when you're \
         in). lait is local-first and end-to-end encrypted.\n"
    );
    let mailto = format!(
        "mailto:{}?subject={}&body={}",
        addr,
        percent_encode(subject),
        percent_encode(&body)
    );
    open_url(&mailto)
}

/// Minimal RFC-3986 percent-encoding for mailto query components (unreserved set
/// passes through; everything else is `%XX`). Avoids a url-crate dependency.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Hand a URL to the OS default handler. Uses `rundll32 …FileProtocolHandler` on
/// Windows (robust with `&` in mailto query strings, unlike `cmd start`).
fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("launch OS url handler")?;
    Ok(())
}

fn kind_str(k: &EventKind) -> &'static str {
    match k {
        EventKind::Join => "join",
        EventKind::Presence => "presence",
        EventKind::System => "system",
    }
}

fn print_event(e: &Event) {
    match e.kind {
        // Surface the joiner's short key so an admin can recognize them straight
        // from the log, not just `--json`.
        EventKind::Join => {
            let short: String = e.id.chars().take(8).collect();
            println!("[join] {} ({}): {}", e.nick, short, e.text);
        }
        EventKind::Presence => println!("[presence] {}: {}", e.nick, e.text),
        EventKind::System => println!("[system] {}: {}", e.nick, e.text),
    }
}

/// Build the per-OS shell invocation for a `watch --exec` hook. `sh -c` doesn't
/// exist on stock Windows, so a hook there silently failed to start; use the
/// native `cmd /C` instead (mirrors how `copy_to_clipboard`/`open_url` split).
fn hook_command(cmd: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

fn run_hook(cmd: &str, e: &Event) {
    let json = serde_json::to_string(e).unwrap_or_default();
    let mut command = hook_command(cmd);
    let child = command
        .env("LAIT_EVENT_SEQ", e.seq.to_string())
        .env("LAIT_EVENT_KIND", kind_str(&e.kind))
        .env("LAIT_EVENT_NICK", &e.nick)
        .env("LAIT_EVENT_ID", &e.id)
        .env("LAIT_EVENT_TEXT", &e.text)
        .env("LAIT_EVENT_TS", e.ts.to_string())
        .stdin(Stdio::piped())
        .spawn();
    match child {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(json.as_bytes());
            }
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => eprintln!("watch: hook failed to start: {err}"),
    }
}

/// Wrap `s` as a single-quoted PowerShell string literal (doubling embedded
/// quotes) so an event nick/text can't break out of the notify command.
#[cfg(target_os = "windows")]
fn ps_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn desktop_notify(e: &Event) {
    let title = format!("lait: {}", e.nick);
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification {:?} with title {:?}", e.text, title);
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        // Best-effort tray balloon via PowerShell NotifyIcon — no external module
        // (BurntToast etc.) required, works on stock Windows 10/11.
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             $n = New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon = [System.Drawing.SystemIcons]::Information; \
             $n.Visible = $true; \
             $n.ShowBalloonTip(5000, {}, {}, 'Info'); \
             Start-Sleep -Milliseconds 6000; $n.Dispose()",
            ps_single_quote(&title),
            ps_single_quote(&e.text),
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("notify-send")
            .arg(&title)
            .arg(&e.text)
            .spawn();
    }
}

/// Foreground presence-notification runner (the `watch` command).
///
/// Parks on a streaming [`Request::Subscribe`] and treats the doorbell purely as
/// a **wake signal**: a frame carries a dirty *flag*, never the events, so each
/// `presence_advanced` ring is followed by a `Log{since}` re-read for the
/// authoritative rows.
///
/// Two cursors are in play and they are **not** interchangeable: `cursor` is an
/// `EventLog` seq (what `Log{since}` filters on), while the doorbell carries its
/// own per-session `seq`. We never compare them. The doorbell's `epoch` is the
/// one field that matters here — a change means the daemon restarted, which
/// resets the `EventLog` sequence to 0, voiding our cursor. Rebaselining to 0
/// on an epoch change is what keeps `watch` from going deaf across a restart:
/// the old `Wait` poll loop held its stale high-water and silently matched
/// nothing forever.
pub async fn watch(
    home: &Path,
    since: Option<u64>,
    exec: Option<String>,
    notify: bool,
) -> Result<()> {
    let scope = scope_for_home(home);
    let address = orbit_address_for_home(home)?;
    scope.authorize(&address)?;
    let space_route = ControlRoute::Space {
        address: address.clone(),
    };
    ensure_daemon(home).await?;
    let daemon = crate::daemon::LaitDaemonClient::current()?;
    // Default to the current high-water: `watch` follows from now, not from the
    // start of the daemon's history.
    let mut cursor = match since {
        Some(n) => n,
        None => match daemon
            .request(space_route.clone(), &Request::Log { since: 0 }, None)
            .await?
        {
            Response::Events { last, .. } => last,
            _ => 0,
        },
    };
    eprintln!("watching from seq {cursor} (Ctrl-C to stop)\u{2026}");

    let mut epoch: Option<u64> = None;
    loop {
        let mut sub = match daemon.subscribe_space(space_route.clone(), 0).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("watch: {e}; reconnecting\u{2026}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = ensure_daemon(home).await;
                continue;
            }
        };
        loop {
            let frame = match sub.next().await {
                Ok(Some(f)) => f,
                // EOF or a broken stream: the daemon stopped or restarted. Drop
                // to the outer loop, which respawns it and re-subscribes.
                Ok(None) => break,
                Err(e) => {
                    eprintln!("watch: {e}; reconnecting\u{2026}");
                    break;
                }
            };
            // A new epoch ⇒ a new daemon ⇒ the EventLog seq restarted at 0, so
            // anything we remember is from a log that no longer exists.
            if epoch.is_some_and(|prev| prev != frame.epoch) {
                eprintln!("watch: daemon restarted; rebaselining\u{2026}");
                cursor = 0;
            }
            epoch = Some(frame.epoch);
            // `reset` covers first-frame + doorbell ring-overrun. Our EventLog
            // cursor survives both (only an epoch change voids it), so a reset
            // is just another reason to re-read.
            if !(frame.presence_advanced || frame.reset) {
                continue;
            }
            match daemon
                .request(space_route.clone(), &Request::Log { since: cursor }, None)
                .await
            {
                Ok(Response::Events { events, last }) => {
                    for e in &events {
                        print_event(e);
                        if let Some(cmd) = &exec {
                            run_hook(cmd, e);
                        }
                        if notify {
                            desktop_notify(e);
                        }
                    }
                    cursor = last.max(cursor);
                }
                Ok(_) => {}
                Err(e) => eprintln!("watch: {e}"),
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = ensure_daemon(home).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SpaceId;

    #[test]
    fn client_side_exit_codes_come_from_the_type_not_the_prose() {
        // Classified errors carry their documented code.
        assert_eq!(
            exit_code_for_error(&CliError::not_found("no space matches 'x'").into()),
            2,
        );
        assert_eq!(
            exit_code_for_error(&CliError::unreachable("daemon is deaf").into()),
            3,
        );
        // ...and anything unclassified stays 1, so this is additive rather than a
        // reclassification of every existing `anyhow!`.
        assert_eq!(exit_code_for_error(&anyhow!("something went wrong")), 1);
        // The code must survive `.context()`: callers add context freely, and a
        // wrapped not-found is still a not-found. (This is the whole reason the
        // class is a type and not a prefix on the message.)
        let wrapped = Err::<(), _>(anyhow::Error::from(CliError::not_found("gone")))
            .context("while resolving --orbit")
            .unwrap_err();
        assert_eq!(exit_code_for_error(&wrapped), 2);
    }

    #[test]
    fn destructive_verbs_ask_and_the_rest_do_not() {
        // Space-level destructive operations remain host policy.
        for req in [
            Request::MemberRemove { who: "ada".into() },
            Request::KeyRotate,
        ] {
            assert!(
                destructive_question(&req).is_some(),
                "{req:?} destroys something and must be confirmed",
            );
        }
    }

    #[test]
    fn confirm_never_blocks_without_a_way_to_ask() {
        // `--yes` is the scripted path: yes, without touching the terminal.
        assert_eq!(
            confirm(
                "x?",
                Out {
                    yes: true,
                    ..Out::default()
                }
            ),
            Confirmed::Yes,
        );
        // `--json` is a machine contract — a prompt would corrupt the stream, so
        // it reports CannotAsk instead of asking. The caller turns that into an
        // error naming `--yes`; it must never wait on stdin.
        assert_eq!(
            confirm(
                "x?",
                Out {
                    json: true,
                    ..Out::default()
                }
            ),
            Confirmed::CannotAsk,
        );
        // Under `cargo test` stdin/stdout are not terminals, which is exactly the
        // CI/agent shape: no TTY → CannotAsk, never a silent hang.
        assert_eq!(confirm("x?", Out::default()), Confirmed::CannotAsk);
    }

    #[test]
    fn paint_is_gated_on_color() {
        // Color off → the string passes through untouched (pipes/`--no-color`/
        // `$NO_COLOR`/non-tty stay clean); color on → wrapped in the code + reset.
        assert_eq!(paint(false, ansi::RED, "hi"), "hi");
        let on = paint(true, ansi::RED, "hi");
        assert!(on.starts_with(ansi::RED) && on.ends_with(ansi::RESET) && on.contains("hi"));
    }

    #[test]
    fn exit_code_is_derived_from_typed_kind_not_prose() {
        // A resolution miss → exit 2, regardless of the (rewordable) message.
        assert_eq!(exit_code_for_kind(ErrorKind::NotFound), 2);
        assert_eq!(exit_code_for_kind(ErrorKind::Error), 1);
        // The constructors carry the kind, and it survives a DTO round-trip so a
        // --json consumer / MCP agent sees the same classification.
        let nf = Response::not_found("no issue matches 'ENG-9x'");
        let json = serde_json::to_string(&nf).unwrap();
        assert!(json.contains("\"error_kind\":\"not_found\""));
        match serde_json::from_str::<Response>(&json).unwrap() {
            Response::Error { error_kind, .. } => assert_eq!(error_kind, ErrorKind::NotFound),
            other => panic!("round-trip changed variant: {other:?}"),
        }
        // A legacy error object with no error_kind field defaults to Error (exit 1).
        let legacy: Response =
            serde_json::from_str(r#"{"kind":"error","message":"boom"}"#).unwrap();
        assert!(matches!(
            legacy,
            Response::Error {
                error_kind: ErrorKind::Error,
                ..
            }
        ));
    }

    #[test]
    fn a_directory_selected_cli_scope_cannot_address_a_sibling_orbit() {
        let selected = PathBuf::from("/tmp/lait-cli-selected");
        let sibling = PathBuf::from("/tmp/lait-cli-sibling");
        let space = SpaceId::from_digest([8; 16]);
        let scope = scope_for_home(&selected);
        let own = OrbitAddress::for_store(&selected, space.clone());
        let other = OrbitAddress::for_store(&sibling, space);

        assert!(scope.authorize(&own).is_ok());
        assert!(scope.authorize(&other).is_err());
    }
}
