//! The client half of the host plane.
//!
//! A head — the local app's HTTP surface, the stdio MCP server — is a *client* of
//! the identity-scoped daemon, and this is the plumbing every head stands on:
//! resolve the Orbit a store names, authorize the call against the head's own
//! scope, stand the daemon up if nobody has yet, and carry one request (or one
//! streamed content transfer) across the local socket. It renders nothing; the
//! head decides what its caller sees.
//!
//! The one rule that shapes everything here: **the World-call path pays for
//! nothing it does not need.** Standing the daemon up is an error-path
//! affair — a send that fails is a better probe than a probe, because the
//! healthy case (a daemon that is already listening) then costs exactly one
//! round trip instead of two.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};

use crate::{
    client_action::ClientAction,
    control::{self, ClientRequest, ControlRoute, ForeignDaemon, Request, Response},
    daemon::{ClientScope, LocalOrbitId, OrbitAddress},
    orbits,
};

/// A client-side failure that carries its own exit code.
///
/// Daemon-side failures already travel with a typed `ErrorKind`. This extends
/// the same rule to failures that never reach the daemon — the alternative is a
/// top-level reporter pattern-matching prose. Plain `anyhow` errors stay code
/// `1`, so classifying is opt-in.
#[derive(Debug)]
pub struct Failure {
    /// The documented exit code for this failure.
    pub code: i32,
    pub message: String,
}

impl Failure {
    /// `2` — a selector resolved to nothing. Matches what the daemon returns for
    /// a missing ref, user, or label, so a missing *Orbit* doesn't answer
    /// differently to the same kind of mistake.
    pub fn not_found(message: impl Into<String>) -> Self {
        Failure {
            code: 2,
            message: message.into(),
        }
    }

    /// `3` — the daemon could not be reached, or could not be understood.
    pub fn unreachable(message: impl Into<String>) -> Self {
        Failure {
            code: 3,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// The exit code represented by a client-side error. Split from
/// [`report_error`] so the mapping is testable without a process to exit —
/// `ExitCode` is deliberately opaque and can't be read back.
///
/// An unclassified error is `1`, so the classification is additive.
fn exit_code_for_error(e: &anyhow::Error) -> i32 {
    if let Some(c) = e.downcast_ref::<Failure>() {
        return c.code;
    }
    // Something is listening and no request will ever get through to it: `3`,
    // daemon unreachable, in the sense that matters.
    if e.downcast_ref::<ForeignDaemon>().is_some() {
        return 3;
    }
    1
}

/// Report a startup failure and return the process exit code.
///
/// `main` returning `Result` would hand every error to anyhow's `Termination`
/// impl, which Debug-prints the `Caused by:` chain (leaking postcard/base32
/// internals), ignores `--json`, and exits `1` regardless of what went wrong.
///
/// Under `--json` the failure is the versioned `Response::Error` DTO on
/// **stdout**, because that is where the readiness line would have gone:
/// `viewer/scripts/dev.mjs` reads the first stdout line and checks
/// `kind === "error"` before it looks for `{token, port}`. Prose on stderr and
/// an empty stdout would leave it waiting for a line that never comes.
pub fn report_error(e: &anyhow::Error, json: bool) -> std::process::ExitCode {
    let code = exit_code_for_error(e);
    // The single-line form: "context: cause", never the multi-line chain.
    let message = format!("{e:#}");
    if json {
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
    // The documented codes are 0-3; anything outside would be a classification
    // bug, and `1` is the honest answer for "this failed" either way.
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// The refusal a store-needing mode gets when this directory holds no space.
///
/// Nothing is created implicitly — the decoy-store trap ("joined, but the board
/// is empty") is gone by construction, not by guard — so the refusal has to
/// carry the navigation state it refused on: which local Orbits exist, or, when
/// none do, how a space comes into existence at all.
///
/// The stdio agent head only reaches this after
/// [`crate::config::Selection::resolve_for_agent`]'s sole-Orbit fallback found
/// nothing unambiguous to bind, so a listing with entries here means several
/// Orbits (a choice), or one whose store is gone (a repair).
pub fn no_store_here() -> String {
    use std::fmt::Write;

    let mut out =
        String::from("no lait space in this directory (nothing is created implicitly).\n");
    let known = orbits::list();
    if known.is_empty() {
        out.push_str(
            "\nrun `lait` to open the local app, then found a space or join one from an invite.",
        );
        return out;
    }
    out.push_str("\nlocal Orbits on this machine:\n");
    for entry in &known {
        let name = if entry.name.is_empty() {
            "(unnamed)"
        } else {
            &entry.name
        };
        let _ = writeln!(out, "  \u{2022} {name}  \u{2192}  {}", entry.path);
    }
    out.push_str(
        "\nset LAIT_STORE to one of these paths (an agent config cannot cd), or run `lait` to see them all in the local app.",
    );
    out
}

/// The question a destructive request must answer before it runs, or `None` if
/// the request destroys nothing.
///
/// Keyed on the `Request` so the list lives in exactly one place, whatever asks
/// it: the local app's head hands the string to the browser to put in a modal,
/// and any other head that grows one asks the same question. A second copy of this
/// list, phrased slightly differently, is how two surfaces end up disagreeing
/// about what is dangerous.
pub fn destructive_question(req: &Request) -> Option<String> {
    match req {
        Request::MemberRemove { who } => Some(format!(
            "remove {who} from this space and rotate the space key?"
        )),
        Request::KeyRotate => Some("rotate the space key?".to_string()),
        _ => None,
    }
}

/// Where a spawned daemon's stderr goes. Truncated per spawn (we only spawn when
/// none is running, so it holds exactly the current daemon's life), and inside
/// the daemon's own home, which is `*`-gitignored.
pub fn daemon_log_path(home: &Path) -> PathBuf {
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

/// Ensure the selected identity's process-level Lait daemon is running.
///
/// Selects no Orbit and needs no store: the web viewer calls this with an empty
/// catalog, and formation itself is a request to the daemon this stands up.
///
/// It reports only success or the real reason there is no daemon (a foreign one,
/// a spawn that died). Whether one was *already* listening is deliberately not
/// returned: it used to license a re-send, and it is the wrong question — see
/// [`control::Undelivered`], which answers the right one.
pub async fn ensure_lait_daemon(selection: &crate::config::Selection) -> Result<()> {
    let exe = std::env::current_exe().context("locate own executable")?;
    ensure_lait_daemon_with_executable(selection, &exe).await
}

/// Ensure the selected identity's process-level Lait daemon is running, using
/// `executable` when one has to be started.
///
/// This is the embedded-client counterpart to [`ensure_lait_daemon`]. A Lait
/// head can self-exec, but a client such as Astrolabe is not the daemon binary:
/// it resolves the fixed `lait` sidecar beside itself and hands that trusted
/// path in here. Probing still comes first, so an already-running identity
/// daemon is attached to without touching the sidecar or starting a competitor.
pub async fn ensure_lait_daemon_with_executable(
    selection: &crate::config::Selection,
    executable: &Path,
) -> Result<()> {
    let client = crate::daemon::Client::for_selection(selection)?;
    let daemon_home = client.home();
    match client.probe().await {
        control::Probe::Healthy => return Ok(()),
        // A daemon behind this build is one nothing here can talk to, ever.
        // Take over from it and carry on to the spawn below.
        control::Probe::Foreign {
            replaceable: true, ..
        } => replace_foreign_daemon(&client).await?,
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
    let log_path = daemon_log_path(daemon_home);
    let log = std::fs::File::create(&log_path).ok();
    // Passing the ordinary config root through `--home` would collapse the
    // global catalog into a self-contained identity, so pin only an explicitly
    // self-contained home — the one this invocation selected, or an ambient one.
    let identity = selection.self_contained_home();
    let mut child = crate::daemon_spawn::spawn(executable, log, identity.as_deref())
        .context("spawn Lait daemon")?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if matches!(client.probe().await, control::Probe::Healthy) {
            // Ours to start, ours to reap. Dropping the handle here instead
            // would leave the daemon's eventual exit uncollected, and a head
            // that outlives its daemon would keep the corpse listed for its own
            // lifetime — see `DaemonChild::reap`.
            child.reap();
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
            // it needs, it just isn't ours — so ask before blaming.
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
    // Giving up on the wait is not giving up on the corpse: a daemon that never
    // answered is exactly the one most likely to exit on its own later.
    child.reap();
    Err(Failure::unreachable(format!(
        "Lait daemon did not come online within 20s — it is running but not answering.\n\
         see {log}, or run `lait daemon` in the foreground to watch it start.",
        log = log_path.display(),
    ))
    .into())
}

/// Stop a daemon this build is ahead of, so this build can start its own.
///
/// Run without asking, and that is the change: the repair used to be an
/// interactive prompt on a command surface that no longer exists. Nothing left
/// has a terminal to ask at, and the situation is not a judgement call — a
/// daemon below `MIN_SUPPORTED_CONTROL_PROTOCOL` cannot answer a single request
/// from this process, so leaving it up leaves the node permanently unusable
/// with no verb anywhere to fix it.
///
/// Only ever called for [`ForeignDaemon::replaceable`], which is *only* true
/// when the peer is behind us. Stopping one that is ahead would be a downgrade
/// at best and an unreadable store at worst; there the handshake's own answer
/// (upgrade this build) is the only honest one.
async fn replace_foreign_daemon(client: &crate::daemon::Client) -> Result<()> {
    let home = client.home().to_path_buf();
    // Read the pid before asking it to go: a daemon that honours `stop` takes
    // its lock file — and the pid stamped in it — with it.
    let pid = crate::config::daemon_pid(&home);
    let _ = client
        .request(ControlRoute::Daemon, &Request::Stop, None)
        .await;
    if daemon_gone(&home).await {
        return Ok(());
    }
    // No signal escalation. `stop` is the one request every version of this
    // protocol has answered, so a daemon that ignores it is not merely old —
    // it is wedged — and killing a pid read from a file is a different risk
    // class than asking a process that is still answering to leave. Name the
    // process instead, which is the one thing an operator needs.
    let named = pid
        .map(|pid| format!(" (pid {pid})"))
        .unwrap_or_else(|| " (its lock file names no pid)".to_string());
    Err(Failure::unreachable(format!(
        "the Lait daemon at {home}{named} is too old for this build and ignored `stop` — \
         end that process and re-run",
        home = home.display(),
    ))
    .into())
}

/// Wait a few seconds for a home's control channel to go quiet.
async fn daemon_gone(home: &Path) -> bool {
    for _ in 0..30 {
        if matches!(control::probe(home).await, control::Probe::Absent) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Send one already-routed request, standing the daemon up only if it turns out
/// not to be there.
///
/// The order is the point. Probing first cost every call a full round trip
/// before the real one, on the overwhelmingly common path where a daemon is
/// already listening and the probe's own doc says the answer is
/// sub-millisecond — two round trips to learn what the first one would have
/// told us. So the send *is* the probe: on failure we run
/// [`ensure_lait_daemon`], which reports the real reason (a foreign daemon, a
/// spawn that died), and re-send **only** what never went out.
///
/// That last clause is the safety of the whole arrangement, and it turns on
/// [`control::Undelivered`] — where the failure happened — not on whether a
/// daemon was listening afterwards. A daemon that applied this request and then
/// exited (another head sent `HostRestart`, or it crashed) leaves nothing
/// listening, so "nobody was home" is precisely the state a
/// double-applied write leaves behind.
pub async fn request_daemon(
    daemon: &crate::daemon::Client,
    selection: &crate::config::Selection,
    envelope: &ClientRequest,
) -> Result<Response> {
    match daemon.send(envelope).await {
        Ok(response) => return Ok(response),
        Err(first) => {
            if !control::undelivered(&first) {
                return Err(applied_or_lost(first).into());
            }
            ensure_lait_daemon(selection).await?;
        }
    }
    daemon
        .send(envelope)
        .await
        .map_err(|error| Failure::unreachable(format!("{error:#}")).into())
}

/// The failure a caller gets when the request went out and no answer came back.
///
/// Unreachable, like any other lost daemon — but it says the one thing the
/// caller cannot work out for itself: the request may have been applied, so
/// re-running it is a second write, not a retry.
fn applied_or_lost(error: anyhow::Error) -> Failure {
    Failure::unreachable(format!(
        "{error:#} — the request had already been sent, so it may have been applied; \
         check before sending it again"
    ))
}

/// Send one host-plane request to this identity's daemon.
///
/// Daemon-scoped, so it selects no Orbit and needs no store: this is the path
/// formation, entry, settings, and device enrolment take, and every one of them
/// runs at a moment when there may be nothing on disk but an identity.
pub async fn host_request(
    daemon: &crate::daemon::Client,
    selection: &crate::config::Selection,
    request: Request,
) -> Result<Response> {
    request_daemon(
        daemon,
        selection,
        &ClientRequest::routed(request, ControlRoute::Daemon, None),
    )
    .await
}

/// The single-Orbit scope implied by store resolution.
pub fn scope_for_home(home: &Path) -> ClientScope {
    ClientScope::pinned(LocalOrbitId::for_store(home))
}

/// Resolve the complete typed address for the one Orbit under `home`.
///
/// This is also the store-presence check: a home with no `ws_` directory has no
/// Orbit to address, so no caller needs a second `read_dir` to ask whether a
/// space is there. A home holding several is not that — it has an Orbit, and no
/// way to say which — so it answers as a plain failure rather than as absence.
pub fn orbit_address_for_home(home: &Path) -> Result<OrbitAddress> {
    match crate::orbital::discover_space(home) {
        crate::orbital::SpaceStore::One(space) => Ok(OrbitAddress::for_store(home, space)),
        crate::orbital::SpaceStore::Absent => {
            Err(Failure::not_found(format!("no local Orbit under {}", home.display())).into())
        }
        crate::orbital::SpaceStore::Several => Err(anyhow::anyhow!(
            "{} holds more than one orbital Space; a home binds one",
            home.display()
        )),
    }
}

/// Send a request through a caller scope derived by a trusted client adapter.
///
/// The allowed set never rides on the wire: authorizing locally chooses an
/// explicit Orbit route, and the receiving StationHost independently validates
/// that it occupies the named Orbit and Space.
pub async fn client_as_scoped(
    home: &Path,
    req: Request,
    scope: &ClientScope,
    act_as: Option<&str>,
    selection: &crate::config::Selection,
) -> Result<Response> {
    client_action_as_scoped(
        home,
        ClientAction::from_legacy(req),
        scope,
        act_as,
        selection,
    )
    .await
}

/// Send an action whose terminal owner was fixed by its caller.
pub async fn client_action_as_scoped(
    home: &Path,
    action: ClientAction,
    scope: &ClientScope,
    act_as: Option<&str>,
    selection: &crate::config::Selection,
) -> Result<Response> {
    let address = orbit_address_for_home(home)?;
    scope.authorize(&address)?;
    let daemon = crate::daemon::Client::for_selection(selection)?;
    let route = action.route(address);
    let request = action
        .into_request()
        .ok_or_else(|| anyhow!("World actions must be dispatched through their client package"))?;
    let envelope = ClientRequest::routed(request, route, act_as.map(str::to_string));
    request_daemon(&daemon, selection, &envelope).await
}

/// Generic facilities supplied by the Lait navigation shell to one client
/// package invocation.
pub struct PackageClientHost {
    home: PathBuf,
    /// The Orbit this host addresses, resolved once by whoever built it.
    ///
    /// Carried rather than re-derived: a head that already knows which Orbit a
    /// request is for (`serve` reads it off its catalog) would otherwise pay a
    /// directory scan of the store root on every World call to learn what it
    /// just told us.
    address: OrbitAddress,
    scope: ClientScope,
    act_as: Option<String>,
    /// Which identity's daemon this invocation talks to.
    selection: crate::config::Selection,
}

impl PackageClientHost {
    pub fn new(
        home: impl Into<PathBuf>,
        address: OrbitAddress,
        scope: ClientScope,
        act_as: Option<String>,
        selection: crate::config::Selection,
    ) -> Self {
        Self {
            home: home.into(),
            address,
            scope,
            act_as,
            selection,
        }
    }

    /// Build a host for the single Orbit under `home`, resolving its address.
    pub fn for_home(
        home: &Path,
        act_as: Option<String>,
        selection: crate::config::Selection,
    ) -> Result<Self> {
        let address = orbit_address_for_home(home)?;
        Ok(Self::new(
            home,
            address,
            scope_for_home(home),
            act_as,
            selection,
        ))
    }
}

impl world_interface::ClientHost for PackageClientHost {
    fn local_root(&self) -> &Path {
        &self.home
    }

    fn call_world<'a>(
        &'a self,
        call: runtime::world::call::Call,
    ) -> world_interface::ClientFuture<'a, runtime::world::call::Reply> {
        Box::pin(async move {
            self.world_reply(call)
                .await
                .map_err(|error| world_interface::Failure::new(format!("{error:#}")))
        })
    }

    fn call_find<'a>(
        &'a self,
        world: replica::body::WorldId,
        query: runtime::find::Query,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let response = client_as_scoped(
                &self.home,
                Request::Find {
                    world: world.as_str().to_owned(),
                    query,
                },
                &self.scope,
                self.act_as.as_deref(),
                &self.selection,
            )
            .await
            .map_err(|error| world_interface::Failure::new(format!("{error:#}")))?;
            match response {
                Response::Find { answer } => serde_json::to_value(answer).map_err(|error| {
                    world_interface::Failure::new(format!("encode Runtime Find answer: {error}"))
                }),
                response @ Response::Error { .. } => {
                    serde_json::to_value(response).map_err(|error| {
                        world_interface::Failure::new(format!(
                            "encode Runtime Find refusal: {error}"
                        ))
                    })
                }
                other => Err(world_interface::Failure::new(format!(
                    "Runtime Find request returned an unexpected response: {other:?}"
                ))),
            }
        })
    }

    fn call_work<'a>(
        &'a self,
        request: runtime::exec::WorkRequest,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let operation =
                data_encoding::HEXLOWER.encode(&runtime::world::RequestId::mint().as_bytes());
            let response = client_as_scoped(
                &self.home,
                Request::Work { request, operation },
                &self.scope,
                self.act_as.as_deref(),
                &self.selection,
            )
            .await
            .map_err(|error| world_interface::Failure::new(format!("{error:#}")))?;
            match response {
                Response::Work { reply } => serde_json::to_value(reply).map_err(|error| {
                    world_interface::Failure::new(format!("encode Runtime Work reply: {error}"))
                }),
                response @ Response::Error { .. } => {
                    serde_json::to_value(response).map_err(|error| {
                        world_interface::Failure::new(format!(
                            "encode Runtime Work refusal: {error}"
                        ))
                    })
                }
                other => Err(world_interface::Failure::new(format!(
                    "Runtime Work request returned an unexpected response: {other:?}"
                ))),
            }
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
            let response = client_as_scoped(
                &self.home,
                request,
                &self.scope,
                self.act_as.as_deref(),
                &self.selection,
            )
            .await
            .map_err(|error| world_interface::Failure::new(format!("{error:#}")))?;
            serde_json::to_value(response).map_err(|error| {
                world_interface::Failure::new(format!("encode host control response: {error}"))
            })
        })
    }

    fn call_identity<'a>(
        &'a self,
        handles: Vec<world_interface::PresentationHandle>,
    ) -> world_interface::ClientFuture<'a, world_interface::PresentationResolution> {
        Box::pin(async move {
            use world_interface::{PresentationResolution, MAX_PRESENTATION_HANDLES};

            if handles.len() > MAX_PRESENTATION_HANDLES {
                return Ok(PresentationResolution::unavailable());
            }
            if self.scope.authorize(&self.address).is_err() {
                return Ok(PresentationResolution::unavailable());
            }
            let wires: Vec<String> = handles
                .iter()
                .map(|handle| handle.to_wire(Some(self.address.space.as_str())))
                .collect();
            let daemon = match crate::daemon::Client::for_selection(&self.selection) {
                Ok(daemon) => daemon,
                Err(_) => return Ok(PresentationResolution::unavailable()),
            };
            let response = match host_request(
                &daemon,
                &self.selection,
                Request::BookResolve {
                    orbit: self.address.orbit.to_string(),
                    handles: wires,
                },
            )
            .await
            {
                Ok(response) => response,
                Err(_) => return Ok(PresentationResolution::unavailable()),
            };
            Ok(presentation_from_book(response))
        })
    }

    fn call_content<'a>(
        &'a self,
        request: world_interface::HostContentRequest,
    ) -> world_interface::ClientFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let fail = |error: anyhow::Error| world_interface::Failure::new(format!("{error:#}"));
            self.scope
                .authorize(&self.address)
                .map_err(|error| fail(anyhow!("{error:#}")))?;
            // A content transfer is a multi-frame conversation, so unlike a
            // one-shot request it cannot use its own failure as the probe: a
            // half-written upload is not re-sendable. This one keeps the eager
            // ensure, and pays one probe per transfer rather than per call.
            ensure_lait_daemon(&self.selection).await.map_err(&fail)?;
            let daemon = crate::daemon::Client::for_selection(&self.selection).map_err(&fail)?;
            let route = crate::control::station_route(self.address.clone());
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

impl PackageClientHost {
    /// Send one package-owned call without decoding its opaque reply here.
    async fn world_reply(
        &self,
        call: runtime::world::call::Call,
    ) -> Result<runtime::world::call::Reply> {
        self.scope.authorize(&self.address)?;
        let route = ControlRoute::World {
            address: self.address.clone(),
            world: call.world().as_str().to_string(),
        };
        let envelope = crate::control::WorldClientRequest::new(route, call, self.act_as.clone());
        let daemon = crate::daemon::Client::for_selection(&self.selection)?;
        match daemon.call_world_envelope(&envelope).await {
            Ok(reply) => Ok(reply),
            // Same rule as `request_daemon`, and for the same reason: a World
            // call is a write as often as not, and only a call that never left
            // this process may be sent again.
            Err(first) => {
                if !control::undelivered(&first) {
                    return Err(applied_or_lost(first).into());
                }
                ensure_lait_daemon(&self.selection).await?;
                daemon
                    .call_world_envelope(&envelope)
                    .await
                    .map_err(|error| Failure::unreachable(format!("{error:#}")).into())
            }
        }
    }
}

/// Map a daemon book-resolution into presentation labels. Card ids stay
/// behind this boundary — a product never sees one.
fn presentation_from_book(response: Response) -> world_interface::PresentationResolution {
    use world_interface::{PresentationLabel, PresentationResolution};

    let Response::BookResolution(view) = response else {
        return PresentationResolution::unavailable();
    };
    PresentationResolution {
        coverage: view.coverage,
        labels: view
            .hits
            .into_iter()
            .filter_map(|hit| {
                let handle = presentation_handle_from_wire(&hit.handle)?;
                let name = (!hit.name.is_empty()).then_some(hit.name);
                Some(PresentationLabel { handle, name })
            })
            .collect(),
    }
}

fn presentation_handle_from_wire(raw: &str) -> Option<world_interface::PresentationHandle> {
    use world_interface::PresentationHandle;
    if let Some(rest) = raw.strip_prefix("actor:") {
        let (space, actor) = rest.split_once(':')?;
        if space.is_empty() || actor.is_empty() {
            return None;
        }
        return Some(PresentationHandle::Actor {
            space: Some(space.to_owned()),
            actor: actor.to_owned(),
        });
    }
    if raw.starts_with("dev_") {
        return Some(PresentationHandle::device(raw));
    }
    None
}

/// Stream a local file onto the content plane.
///
/// Read in pieces and forwarded as they arrive: a file larger than memory is
/// the case this whole plane exists for, so materialising it here would defeat
/// the purpose one layer above where it matters.
async fn content_write(
    home: &Path,
    route: ControlRoute,
    path: &Path,
) -> Result<serde_json::Value, world_interface::Failure> {
    use tokio::io::AsyncReadExt;

    let fail = world_interface::Failure::new;
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
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| fail("file reader returned an invalid byte count".to_string()))?;
        upload
            .push(chunk)
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
    route: ControlRoute,
    content: &str,
    destination: &Path,
) -> Result<serde_json::Value, world_interface::Failure> {
    use tokio::io::AsyncWriteExt;

    let fail = world_interface::Failure::new;
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
                    len: content_range_bytes(),
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
        offset = offset.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
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

/// The read window one content frame carries, as a wire-sized integer.
fn content_range_bytes() -> u64 {
    u64::try_from(runtime::plane::freight::content::MAX_RANGE_BYTES).unwrap_or(u64::MAX)
}

async fn content_stat(
    home: &Path,
    route: ControlRoute,
    content: &str,
) -> Result<serde_json::Value, world_interface::Failure> {
    let fail = world_interface::Failure::new;
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

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::ids::SpaceId;

    #[test]
    fn client_side_exit_codes_come_from_the_type_not_the_prose() {
        assert_eq!(
            exit_code_for_error(&Failure::not_found("no space matches 'x'").into()),
            2,
        );
        assert_eq!(
            exit_code_for_error(&Failure::unreachable("daemon is deaf").into()),
            3,
        );
        // ...and anything unclassified stays 1, so this is additive rather than a
        // reclassification of every existing `anyhow!`.
        assert_eq!(exit_code_for_error(&anyhow!("something went wrong")), 1);
        // The code must survive `.context()`: callers add context freely, and a
        // wrapped not-found is still a not-found. (This is the whole reason the
        // class is a type and not a prefix on the message.)
        let wrapped = Err::<(), _>(anyhow::Error::from(Failure::not_found("gone")))
            .context("while resolving --orbit")
            .unwrap_err();
        assert_eq!(exit_code_for_error(&wrapped), 2);
    }

    #[test]
    fn a_directory_selected_scope_cannot_address_a_sibling_orbit() {
        let selected = PathBuf::from("/tmp/lait-client-selected");
        let sibling = PathBuf::from("/tmp/lait-client-sibling");
        let space = SpaceId::from_digest([8; 16]);
        let scope = scope_for_home(&selected);
        let own = OrbitAddress::for_store(&selected, space.clone());
        let other = OrbitAddress::for_store(&sibling, space);

        assert!(scope.authorize(&own).is_ok());
        assert!(scope.authorize(&other).is_err());
    }

    /// A re-send is licensed by where the failure happened, never by whether a
    /// daemon turned out to be listening afterwards.
    ///
    /// The bug this pins: a daemon that applied the request and then exited
    /// leaves nothing listening, so "nobody was home" used to read as "it never
    /// arrived" and the write went in twice.
    #[tokio::test]
    async fn only_a_request_that_never_went_out_licenses_a_resend() {
        // Nothing is listening at this home, so the send fails at connect.
        let nowhere = std::env::temp_dir().join("lait-no-daemon-here");
        let envelope = ClientRequest::routed(Request::Status, ControlRoute::Daemon, None);
        let failure = control::send(&nowhere, &envelope)
            .await
            .expect_err("no daemon is listening there");
        assert!(
            control::undelivered(&failure),
            "a connect that never opened carried no request: {failure:#}",
        );

        // A reply that never came back is not the same fact, whatever the
        // message says, and must not be re-sent.
        let lost = anyhow!("read response: connection reset").context("while asking the daemon");
        assert!(!control::undelivered(&lost));
        assert_eq!(
            exit_code_for_error(&applied_or_lost(lost).into()),
            3,
            "a lost reply is still unreachable — it just may have applied",
        );
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
        assert!(destructive_question(&Request::Status).is_none());
    }

    /// The typed kind survives the wire, so every surface classifies the same
    /// failure the same way — the browser's 409/404 split and this module's
    /// exit codes both read the type, never the prose.
    #[test]
    fn a_not_found_stays_a_not_found_across_the_dto() {
        let json = serde_json::to_string(&Response::not_found("no issue matches 'ENG-9x'"))
            .expect("serialize");
        assert!(json.contains("\"error_kind\":\"not_found\""), "{json}");
        match serde_json::from_str::<Response>(&json).expect("deserialize") {
            Response::Error { error_kind, .. } => {
                assert_eq!(error_kind, crate::control::ErrorKind::NotFound);
            }
            other => panic!("round-trip changed variant: {other:?}"),
        }
        // A legacy error object with no error_kind field defaults to Error.
        let legacy: Response =
            serde_json::from_str(r#"{"kind":"error","message":"boom"}"#).expect("legacy");
        assert!(matches!(
            legacy,
            Response::Error {
                error_kind: crate::control::ErrorKind::Error,
                ..
            }
        ));
    }

    #[test]
    fn a_book_resolution_crosses_as_labels_not_cards() {
        let resolution = presentation_from_book(Response::BookResolution(Box::new(
            crate::control::BookResolutionView {
                hits: vec![crate::control::BookHitView {
                    card: "crd_secret".into(),
                    handle: "actor:ws_one:act_ada".into(),
                    name: "Ada".into(),
                    picture: None,
                }],
                coverage: None,
            },
        )));
        assert!(!resolution.is_unavailable());
        assert_eq!(resolution.name_for_actor("act_ada"), Some("Ada"));
        assert!(
            !format!("{resolution:?}").contains("crd_secret"),
            "a Card id leaked into presentation: {resolution:?}"
        );
    }

    #[test]
    fn an_error_or_wrong_variant_is_unavailable_not_empty_names() {
        let resolution = presentation_from_book(Response::err("no"));
        assert!(resolution.is_unavailable());
        assert!(resolution.labels.is_empty());
    }

    #[test]
    fn an_empty_card_name_stays_absent() {
        let resolution = presentation_from_book(Response::BookResolution(Box::new(
            crate::control::BookResolutionView {
                hits: vec![crate::control::BookHitView {
                    card: "crd_one".into(),
                    handle: "actor:ws_one:act_ada".into(),
                    name: String::new(),
                    picture: None,
                }],
                coverage: None,
            },
        )));
        assert_eq!(resolution.name_for_actor("act_ada"), None);
    }
}
