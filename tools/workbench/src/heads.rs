//! Heads: the browser and MCP adapters attached to a daemon.
//!
//! A head is a first-class object the client lists, starts and stops — not an
//! incidental consequence of starting a device. The difference shows up the
//! moment something goes wrong: an agent whose MCP binding is missing or stale
//! does not fail loudly, it simply has no tools, and nobody finds out until
//! somebody looks.
//!
//! ## Two kinds, and only one of them is ours
//!
//! A **browser head** is `lait --json`, which this supervisor spawns and holds
//! a handle to. It prints one machine-readable readiness line before it accepts
//! a connection, which is what makes starting one a bounded operation rather
//! than a poll.
//!
//! An **MCP head** is `lait mcp`, and it is *structurally external*: the agent's
//! harness spawns it as its own child, so this process never holds that handle
//! and can only reach it through verified attach. Modelling both as "a head"
//! while keeping their ownership honest is the whole job of this module.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// How long to wait for a browser head to announce itself.
///
/// It prints its readiness line before the listener accepts, so this is a
/// startup budget rather than a network timeout — generous enough for a cold
/// process on a loaded machine, short enough that a head which will never
/// answer is reported rather than waited on.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a head gets to shut down on its own before it is forced.
///
/// Its ordered shutdown releases streaming responses and joins its tasks, so this
/// is bounded by how long a drain takes rather than by anything a network does. Too
/// short and every stop is a force; too long and a wedged head holds a person's
/// click.
const GRACEFUL_STOP_BUDGET: Duration = Duration::from_secs(5);

/// How often the stop ladder checks whether the ask was taken.
const STOP_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeadFacts {
    pub id: String,
    pub kind: HeadKind,
    /// The managed device whose daemon this head is attached to, when there is
    /// one.
    ///
    /// `None` is the person's *own* identity daemon — the always-running local
    /// service this supervisor attaches to rather than manages. Naming a device
    /// there would claim a registration that does not exist, and the Library's
    /// `Open` runs entirely through that case: the Orbits a person sees belong
    /// to their identity, not to the development fleet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The Orbit it is bound to, when it is bound to one. A browser head serves
    /// every Orbit the identity has; an MCP head is authored against one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orbit: Option<String>,
    /// The one World this head serves.
    ///
    /// `None` is a head that predates the pin and answers for every mounted
    /// World. That is a fact about the head, not a default to paper over: a
    /// supervisor cannot make a definite statement about such a head, and
    /// pretending otherwise is the whole defect this field exists to end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    pub identity: String,
    pub ownership: Ownership,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Where a browser should go. Carries the run credential, which is why it
    /// is handed to a person rather than logged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What can be said about this head *now*, rather than when it started.
    ///
    /// This field exists because its absence was the one place in this tree where
    /// a guess was reported as a fact. `HeadFacts` was recorded at spawn and
    /// cloned out on every list, so a head that had crashed read as running
    /// forever, and `stop` on it answered success — leaving a person unable to
    /// tell "I stopped it" from "it had already died".
    pub state: HeadState,
}

/// What the supervisor can say about a head right now.
///
/// # This is liveness, not readiness, and the distinction is deliberate
///
/// [`HeadState::Running`] means the process has not exited. It does **not** mean
/// the head answers. A head that is alive and wedged reads `Running` here, and
/// that is the honest limit of what a free check can establish.
///
/// Separating the two is the settled answer to this problem — Kubernetes splits
/// liveness from readiness for the reason that a single verdict makes an outage
/// in a dependency indistinguishable from a dead process, so the remedy for one
/// gets applied to the other. Here the split falls out of cost: a `try_wait` is
/// free and can run on every list, while asking a head whether it answers is a
/// round trip per head per refresh. This codebase already refuses that trade in
/// the same words — a Library that fetched something per row to draw itself would
/// make listing cost what opening costs.
///
/// So readiness is asked when somebody acts, not when a list is drawn, and this
/// enum promises only what it can keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum HeadState {
    /// The process has not exited. Says nothing about whether it answers.
    Running,
    /// The process exited, and this is what it exited with.
    ///
    /// The entry stays listed rather than vanishing, because a row that
    /// disappears looks exactly like one that was never started — and the person
    /// needs to learn that the thing they opened died.
    Exited { status: String },
    /// It could not be determined, and this is why.
    ///
    /// Never collapsed into either of the others. A poll that failed is not a
    /// dead process and not a live one, and this is the arm that keeps the
    /// running answer from being the only lifecycle statement in the tree that
    /// cannot say "I could not ask".
    Unknown { why: String },
}

impl HeadState {
    /// Whether a caller may treat this head as one it can still use.
    ///
    /// `Unknown` answers `false`: a head nobody could poll is not a head to hand
    /// somebody a URL for. The failure that matters is the other direction —
    /// reporting usable when it is not — so uncertainty resolves against use, the
    /// same way `update::world::Standing::behind` answers `false` for every
    /// uncertainty.
    pub fn usable(&self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeadKind {
    Browser,
    Mcp,
}

/// Whether this process can prove it created the head.
///
/// The same boundary daemons have, for the same reason, and it is not a
/// formality here: an MCP head is *always* external, because the harness that
/// spawns it is the agent's and not ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Owned,
    External,
}

/// What a head announced about itself when it came up.
#[derive(Debug, Clone, Deserialize)]
struct Ready {
    url: String,
    #[allow(
        dead_code,
        reason = "the URL already carries it; kept so the line's shape is asserted"
    )]
    token: String,
    #[allow(dead_code, reason = "as above")]
    port: u16,
    /// Which World this head came up serving.
    ///
    /// Read from the head rather than remembered from the arguments it was
    /// given: the head resolves the pin, so the head is the one that knows. A
    /// supervisor that recorded its own intent instead would be right until the
    /// day the resolution disagreed with it — which is the day it matters.
    ///
    /// Defaulted so a head from a build before the pin still parses; such a head
    /// answers for every World, which is what an empty string says here.
    #[serde(default)]
    world: String,
}

/// What stopping a head actually did.
///
/// Two successes, deliberately distinguishable. "I stopped it" and "it had
/// already died" are different facts about the same request, and the second is
/// one a person needs — it is the only signal that the World they opened fell over
/// on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// It was asked, and it went.
    Stopped,
    /// It was asked, did not go inside the budget, and was forced.
    ///
    /// Distinct from [`Stopped::Stopped`] because it is the one that means
    /// something got cut off: a head that had to be killed did not run its
    /// ordered shutdown, so a browser saw a reset and a transfer was severed.
    /// Reporting it as an ordinary stop would hide the only evidence that
    /// anything was lost.
    Forced,
    /// It had already exited before the request arrived.
    WasAlreadyGone { status: String },
}

/// A browser head this process started and holds.
pub(crate) struct OwnedHead {
    facts: HeadFacts,
    child: Child,
}

impl OwnedHead {
    pub(crate) fn facts(&self) -> &HeadFacts {
        &self.facts
    }

    /// Poll the handle and answer what it says.
    ///
    /// Mirrors `Supervisor::refresh_owned_process` for devices, including the arm
    /// that matters most: a poll that *fails* records why and does not decide the
    /// head is dead. Three answers, because there are three facts.
    ///
    /// `&mut` rather than consuming, because unlike `stop` this is something a
    /// list does — the whole defect was that listing never asked.
    pub(crate) fn refresh(&mut self) -> HeadState {
        self.facts.state = match self.child.try_wait() {
            Ok(Some(exit)) => HeadState::Exited {
                status: format!("{exit}"),
            },
            Ok(None) => HeadState::Running,
            Err(error) => HeadState::Unknown {
                why: format!("poll head process: {error}"),
            },
        };
        self.facts.state.clone()
    }

    /// Stop it, and collect it. Only ever called on a head we spawned — the
    /// handle *is* the proof, exactly as it is for a daemon.
    pub(crate) fn stop(mut self) -> Result<Stopped> {
        self.stop_within(GRACEFUL_STOP_BUDGET)
    }

    /// Ask, wait, then force — the ladder `stop_device` already climbs.
    ///
    /// This used to be `kill()` alone, and what that threw away is specific. The
    /// head has a complete ordered shutdown (`serve::run_until`): it flips its stop
    /// channel *before* axum begins draining, so the never-completing SSE and
    /// WebSocket responses release, then joins its tasks rather than aborting them
    /// — its own comment says that is "so 'did it stop' is a fact rather than a
    /// hope". A SIGKILL runs none of it: browsers get a reset mid-stream and
    /// in-flight content transfers are severed. Rude for a page; data loss for a
    /// World running a server of its own.
    ///
    /// **Why a signal and not a request.** The obvious alternative — ask the head
    /// over its own HTTP surface — is closed on purpose. `Request::Stop` is refused
    /// on the host plane because "`Stop` reaches whatever process is on the other
    /// end of the socket, and a page that could send it could kill the server
    /// answering it". A graceful stop must therefore travel a channel a web page
    /// cannot reach, and a signal to the process group is that channel.
    ///
    /// The group, not the process: a World that spawned children of its own is
    /// asked as a whole, which is the case a page-sized head does not have and a
    /// server-sized one does.
    pub(crate) fn stop_within(&mut self, budget: Duration) -> Result<Stopped> {
        // Already gone is still a success — the caller asked for it to not be
        // running and it is not — but it is a *different* success, and saying so
        // is the point. A supervisor that answers "stopped" identically either way
        // cannot tell somebody their World had already crashed.
        if let Some(exit) = self.child.try_wait().context("poll head")? {
            return Ok(Stopped::WasAlreadyGone {
                status: format!("{exit}"),
            });
        }

        if request_stop(&self.child) {
            // Poll rather than block: `wait` would give up the ability to escalate,
            // and a head that ignores the ask must not hold the supervisor forever.
            // `checked_add` because this crate denies silent arithmetic. An
            // `Instant` that cannot represent now-plus-a-few-seconds is a machine
            // in a state where waiting is not the useful answer, so it escalates.
            let deadline = Instant::now().checked_add(budget);
            while deadline.is_some_and(|deadline| Instant::now() < deadline) {
                if self.child.try_wait().context("poll head")?.is_some() {
                    return Ok(Stopped::Stopped);
                }
                std::thread::sleep(STOP_POLL);
            }
        }

        // It would not, or could not, be asked. Force is the last rung and it is
        // still reached, because a supervisor that cannot stop a wedged process is
        // not a supervisor.
        //
        // The group, then the process. `Child::kill` signals one process and leaves
        // descendants (rust-lang/rust#115241), so a forced stop that only killed the
        // handle would leave a World's own server running — and this claim was made
        // in `own_process_group`'s doc before the code did it, which is the kind of
        // gap that reads as done. `force_group` answers whether it reached anything;
        // the process kill follows either way, both because it is what reaps the
        // handle and because a group signal that found nothing must not be mistaken
        // for a stop.
        force_group(&self.child);
        self.child.kill().context("stop head")?;
        self.child.wait().context("collect head")?;
        Ok(Stopped::Forced)
    }
}

/// Start a browser head, and wait for it to say where it is.
///
/// `home` selects which *self-contained identity* the head serves, passed as
/// the launcher's `--home` — the same flag the daemon takes, because it selects
/// the same thing. A fleet device has one and it must be given, or the head
/// serves whatever identity the supervisor's own environment happens to name.
///
/// `None` is the ordinary per-user identity, and it is spelled as the absence
/// of the flag rather than as a path. There is no path that means "the ordinary
/// one": passing the config root would collapse the global catalog into a
/// self-contained identity, and passing the daemon's own home would produce a
/// head serving an identity nobody has ever used — which is a head that comes
/// up perfectly and lists nothing.
pub(crate) fn start_browser(
    executable: &Path,
    id: String,
    device: Option<String>,
    home: Option<&Path>,
    world: Option<&str>,
) -> Result<OwnedHead> {
    let mut command = Command::new(executable);
    if let Some(world) = world {
        // One head, one World. Passed rather than left to `$LAIT_WORLD`,
        // because a supervisor starting several must be able to say which is
        // which without a process-wide variable that only one of them could
        // win.
        command.arg("--world").arg(world);
    }
    command
        .arg("--json")
        // An ephemeral port, always. A fixed one turns "start a second head"
        // into a collision whose error names a port rather than the head that
        // already holds it.
        .arg("--port")
        .arg("0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = home {
        command.arg("--home").arg(home);
    }
    no_console(&mut command);
    own_process_group(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn head {}", executable.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("head produced no stdout to read its readiness line from"))?;

    match read_ready(stdout) {
        Ok(ready) => Ok(OwnedHead {
            facts: HeadFacts {
                id,
                kind: HeadKind::Browser,
                device,
                orbit: None,
                // What the head said, not what it was asked for. An empty
                // announcement is a head that predates the pin, and stays
                // `None` rather than being credited with a World it never
                // claimed.
                world: Some(ready.world.clone()).filter(|world| !world.is_empty()),
                identity: home.map_or_else(
                    || "the ordinary identity".to_owned(),
                    |home| home.to_string_lossy().into_owned(),
                ),
                ownership: Ownership::Owned,
                pid: Some(child.id()),
                url: Some(ready.url),
                // It printed its readiness line, so it is up. Every later answer
                // comes from polling the handle rather than from this moment,
                // which is the whole point of the field.
                state: HeadState::Running,
            },
            child,
        }),
        Err(error) => {
            // A head that never announced itself is not left running: it holds
            // the image and a port and answers to nobody, which is the exact
            // shape of the orphan this whole initiative exists to stop making.
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

/// Read the one readiness line, or give up.
///
/// Blocking, on purpose, and called from a blocking context. The line arrives
/// before the head accepts its first connection, so waiting for it is waiting
/// for the head to be usable rather than guessing that it might be.
fn read_ready(stdout: std::process::ChildStdout) -> Result<Ready> {
    // `checked_add` because a clock near the end of its representable range
    // would otherwise wrap the deadline into the past and reject a head that
    // was about to answer perfectly well. `None` is treated as "no deadline",
    // which is the safe direction: the read still ends when the pipe closes.
    let deadline = Instant::now().checked_add(READY_TIMEOUT);
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).context("read head readiness")?;
        if read == 0 {
            return Err(anyhow!("head exited before it announced an address"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(anyhow!("head did not announce an address in time"));
            }
            continue;
        }
        return serde_json::from_str::<Ready>(trimmed)
            .with_context(|| format!("head announced something unreadable: {trimmed}"));
    }
}

/// An `OwnedHead` whose process has already exited, for tests that need to drive
/// the map rather than the launcher.
///
/// Here rather than in the test module that uses it because `OwnedHead`'s fields
/// are private to this module — and they should stay that way. A `pub(crate)`
/// constructor gated to tests is narrower than opening the struct.
#[cfg(test)]
pub(crate) fn dead_head_for_test(id: String) -> OwnedHead {
    let mut child = Command::new(if cfg!(windows) { "cmd" } else { "true" });
    if cfg!(windows) {
        child.args(["/C", "exit 0"]);
    }
    own_process_group(&mut child);
    let mut child = child.spawn().expect("spawn a short-lived process");
    // Waited here so the handle is genuinely dead before the test polls it.
    let _ = child.wait();
    OwnedHead {
        facts: HeadFacts {
            id,
            kind: HeadKind::Browser,
            device: None,
            orbit: None,
            world: Some("issues".to_owned()),
            identity: "test".to_owned(),
            ownership: Ownership::Owned,
            pid: None,
            // The stale URL the defect used to hand back.
            url: Some("http://127.0.0.1:1/?token=stale".to_owned()),
            state: HeadState::Running,
        },
        child,
    }
}

/// SIGKILL the head's whole process group, so descendants go with it.
///
/// Safe only because a daemon is no longer in a head's group: `daemon_spawn` gives
/// the identity daemon its own, for the reason its windows branch always stated. A
/// group kill from here before that fix would have taken the machine's daemon down
/// with a per-World stop.
#[cfg(unix)]
fn force_group(child: &Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    if let Some(group) = pid.checked_neg() {
        if group != 0 {
            // SAFETY: the documented POSIX form. `ESRCH` for a group that has
            // already gone is a value, not undefined behaviour.
            unsafe { libc::kill(group, libc::SIGKILL) };
        }
    }
}

#[cfg(not(unix))]
fn force_group(_child: &Child) {}

/// Ask a head to stop, on the channel a web page cannot reach.
///
/// `false` means no ask was possible on this platform, so the caller should go
/// straight to force rather than waiting out a budget for a message nobody sent.
/// That distinction is why this returns a bool instead of swallowing the case:
/// waiting five seconds for a signal that was never delivered would make every
/// Windows stop feel broken.
#[cfg(unix)]
fn request_stop(child: &Child) -> bool {
    let Ok(pid) = i32::try_from(child.id()) else {
        return false;
    };
    // Negative pid addresses the *group*, which is the one the head was spawned as
    // the root of. That is what reaches a World's own children; signalling the pid
    // alone would ask the supervisor's child and leave its descendants running.
    //
    // SIGTERM, not SIGINT: the head listens for both, and SIGTERM is the one that
    // means "shut down" rather than "the person at a terminal pressed something".
    //
    // SAFETY: the documented POSIX form. `kill` on a group or process that has
    // already exited answers `ESRCH`, which is a value here and not undefined
    // behaviour.
    //
    // The group first, then the process alone. The fallback is not belt-and-braces:
    // `kill(-pid, …)` only resolves if the child actually leads a group, so a head
    // spawned by a path that forgot `own_process_group` would otherwise get *no
    // ask at all* and be forced every time — a silent downgrade from graceful to
    // violent, which is the failure mode this whole ladder exists to remove. Asking
    // the process alone still runs its ordered shutdown; what it misses is a
    // World's own children, which is worth saying rather than worth failing over.
    // The group id is the pid negated, and the negation is checked: `kill(0, …)`
    // means "every process in *our* group", which would signal the supervisor
    // itself. A pid that cannot be negated is one to refuse rather than to guess at.
    if let Some(group) = pid.checked_neg() {
        if group != 0 && unsafe { libc::kill(group, libc::SIGTERM) } == 0 {
            return true;
        }
    }
    unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
}

/// No ask is available yet — see `own_process_group` for exactly what stands in
/// the way and what the shape of the answer is.
#[cfg(not(unix))]
fn request_stop(_child: &Child) -> bool {
    false
}

/// Put a head at the root of its own process group.
///
/// Three things this buys, and the first is a live bug:
///
/// 1. **A terminal's Ctrl-C stops reaching it.** A child inherits its parent's
///    foreground process group, so on unix a Ctrl-C in the terminal that launched
///    the client delivered SIGINT to every head *and* the identity daemon —
///    "stop one World" had a sibling path that stopped everything, and it was the
///    default keystroke. `daemon_spawn`'s windows branch already argues this case
///    in prose ("sharing the spawner's console puts the daemon in that console's
///    process group, so a Ctrl-C or a closed terminal delivers a control event to
///    a process whose whole contract is to outlive the command that started it")
///    while the unix branch did the thing it warns against.
///
/// 2. **It is what a graceful stop can address.** Signalling the group rather
///    than the process is what reaches a World that spawned children of its own —
///    a World running a server, rather than serving a page.
///
/// 3. **It bounds a force-stop.** `Child::kill` signals one process and leaves
///    descendants (rust-lang/rust#115241); a group does not.
///
/// Windows is deliberately not done here, and the reason is recorded rather than
/// left as an omission. `CREATE_NEW_PROCESS_GROUP` would give the same root, and
/// `GenerateConsoleCtrlEvent` with **`CTRL_BREAK_EVENT`** is then the graceful
/// signal — `CTRL_C_EVENT` is documented to *succeed and not be delivered* for a
/// group, which is the trap in every naive port of this. But a console control
/// event only reaches processes attached to the same console as the caller, and a
/// head is spawned `CREATE_NO_WINDOW` precisely so it has none. Reconciling those
/// two needs a Windows machine to verify on, and untested process control in a
/// supervisor is worse than a named gap.
#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // Stable since 1.64. `0` means "a new group whose id is the child's pid",
    // which is what makes the group addressable by the pid we already hold.
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {}

#[cfg(windows)]
fn no_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // Same reason the daemon spawn sets it: a console-subsystem child spawned
    // from a parent with no console gets a freshly allocated one, which is a
    // black window on screen for as long as the head lives.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console(_command: &mut Command) {}

/// An MCP head, described rather than started.
///
/// The agent's harness spawns `lait mcp` as its own child, so this process can
/// never hold that handle. Authoring one is authoring a *binding* — which
/// orbit, which identity, which binary — and `HostInstallMcp` is what writes
/// it. What the client offers is the choosing, and then an honest account of
/// what it can and cannot do about the result.
pub fn mcp_head(
    id: String,
    device: Option<String>,
    identity: PathBuf,
    orbit: Option<String>,
) -> HeadFacts {
    HeadFacts {
        id,
        kind: HeadKind::Mcp,
        device,
        orbit,
        // An MCP head is pinned by its binding rather than by a flag this
        // supervisor passed, and this process never spawns one — so what World
        // it speaks is the binding's fact to state, not ours to claim.
        world: None,
        identity: identity.to_string_lossy().into_owned(),
        // Always. Not "unless we happen to have spawned it" — there is no path
        // on which this process is the parent, and a field that could say
        // otherwise would invite a caller to try stopping it.
        ownership: Ownership::External,
        pid: None,
        url: None,
        // No handle, so there is nothing to poll. Not `Running` — that would be
        // a claim this process cannot support about a child it never had.
        state: HeadState::Unknown {
            why: "an MCP head is spawned by the agent's harness, so this process \
                  holds no handle to poll"
                .to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An MCP head is external by construction, not by circumstance. Nothing
    /// this process does can make it owned, and the type should not suggest
    /// otherwise.
    #[test]
    fn an_mcp_head_is_always_external() {
        let head = mcp_head(
            "agent-1".into(),
            Some("alice".into()),
            PathBuf::from("/home/alice"),
            Some("orb_one".into()),
        );
        assert_eq!(head.ownership, Ownership::External);
        assert_eq!(head.kind, HeadKind::Mcp);
        assert!(
            head.pid.is_none(),
            "an MCP head reported a pid this process cannot have observed"
        );
        assert!(head.url.is_none(), "an MCP head is not a browser head");
    }

    /// A head that never announces an address is not left running. One that
    /// holds the image and a port and answers to nobody is exactly the orphan
    /// this initiative exists to stop making.
    #[test]
    fn a_head_that_never_announces_itself_is_not_left_running() {
        let directory = tempfile::tempdir().expect("tempdir");
        // A program that exits immediately without printing anything stands in
        // for a head that fails to come up.
        let executable = if cfg!(windows) {
            PathBuf::from("cmd.exe")
        } else {
            PathBuf::from("/bin/true")
        };
        let started = start_browser(
            &executable,
            "head-1".into(),
            Some("alice".into()),
            Some(directory.path()),
            None,
        );
        assert!(
            started.is_err(),
            "a process that announced nothing was accepted as a head"
        );
    }

    /// A head that has exited says so, and a stop on it says which success it was.
    ///
    /// This is the test that would have caught the original defect. `HeadFacts`
    /// was recorded at spawn and cloned on every list, so this sequence — start,
    /// let it die, ask — reported `Running` forever, and `stop` answered plain
    /// success. Both halves are asserted here because fixing one without the
    /// other still leaves a person unable to tell what happened.
    #[test]
    fn a_head_that_died_reports_it_and_stopping_it_says_which_success() {
        let mut head = OwnedHead {
            facts: HeadFacts {
                id: "probe".into(),
                kind: HeadKind::Browser,
                device: None,
                orbit: None,
                world: Some("issues".into()),
                identity: "test".into(),
                ownership: Ownership::Owned,
                pid: None,
                url: None,
                // Deliberately the stale claim the defect used to keep forever.
                state: HeadState::Running,
            },
            child: short_lived(),
        };

        // Wait for the real process to exit, so this is a poll of a dead handle
        // rather than a simulated one.
        for _ in 0..200 {
            if matches!(head.refresh(), HeadState::Exited { .. }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        match head.facts().state.clone() {
            HeadState::Exited { status } => {
                assert!(!status.is_empty(), "an exit must carry what it exited with")
            }
            other => panic!("a process that exited reported {other:?}"),
        }
        assert!(
            !head.facts().state.usable(),
            "a dead head must not read as usable"
        );

        match head.stop().expect("stop a dead head") {
            Stopped::WasAlreadyGone { status } => assert!(!status.is_empty()),
            other => panic!("stopping a head that had already exited reported {other:?}"),
        }
    }

    /// A live head reports running, and stopping it reports a stop.
    #[test]
    fn a_live_head_reports_running_and_stops_as_one() {
        let mut head = OwnedHead {
            facts: HeadFacts {
                id: "probe".into(),
                kind: HeadKind::Browser,
                device: None,
                orbit: None,
                world: None,
                identity: "test".into(),
                ownership: Ownership::Owned,
                pid: None,
                url: None,
                state: HeadState::Running,
            },
            child: long_lived(),
        };
        assert_eq!(head.refresh(), HeadState::Running);
        assert!(head.facts().state.usable());
        // On Unix, `sleep` does not handle SIGTERM specially, so the default
        // disposition terminates it — exactly the ordinary case: asked, and it
        // went.
        //
        // On Windows there is no ask to make. `request_stop` answers `false`
        // there and the ladder goes straight to force by design, rather than
        // waiting out a budget for a message nobody sent. So the healthy stop
        // of a healthy process reports `Forced`, and that is the honest
        // answer rather than a defect: what the platform lacks is the rung,
        // not the shutdown. Asserting `Stopped` here would demand behaviour
        // the code deliberately does not have.
        let expected = if cfg!(unix) {
            Stopped::Stopped
        } else {
            Stopped::Forced
        };
        assert_eq!(head.stop().expect("stop"), expected);
    }

    /// A head that ignores the ask is forced, and says it was forced.
    ///
    /// The rung that matters most, because it is the one a wedged World takes. If
    /// force were unreachable a supervisor could not stop anything that stopped
    /// listening; if it reported an ordinary stop, the evidence that a shutdown was
    /// cut short would be gone.
    ///
    /// A tiny budget rather than the production five seconds: this asserts the
    /// escalation happens, and waiting five real seconds to prove a timeout is a
    /// test that costs more than it proves.
    #[test]
    fn a_head_that_ignores_the_ask_is_forced_and_reports_it() {
        let mut head = OwnedHead {
            facts: HeadFacts {
                id: "deaf".into(),
                kind: HeadKind::Browser,
                device: None,
                orbit: None,
                world: Some("issues".into()),
                identity: "test".into(),
                ownership: Ownership::Owned,
                pid: None,
                url: None,
                state: HeadState::Running,
            },
            child: deaf_to_term(),
        };

        assert_eq!(
            head.refresh(),
            HeadState::Running,
            "the fixture must be alive"
        );
        assert_eq!(
            head.stop_within(Duration::from_millis(250))
                .expect("stop a deaf head"),
            Stopped::Forced,
            "a head that ignored SIGTERM must be forced, and must say so"
        );
    }

    /// A single process that ignores SIGTERM, so the ladder must escalate.
    ///
    /// `exec` is load-bearing, and the first attempt without it is instructive: a
    /// plain `sh -c "trap '' TERM; sleep 60"` is *two* processes, and signalling the
    /// group reached the `sleep`, which was not deaf — so the shell's `wait`
    /// returned, the tree exited, and the ladder reported an ordinary stop. That is
    /// the group signalling working, and it is now asserted on its own below.
    ///
    /// `trap '' TERM; exec sleep 60` leaves one process instead. An *ignored*
    /// disposition survives `exec` (POSIX: only handled signals reset to default),
    /// so this is a `sleep` that cannot be asked — which is the case the budget and
    /// the force rung exist for.
    #[cfg(unix)]
    fn deaf_to_term() -> Child {
        // It announces itself before it becomes deaf, and this test waits for that
        // line. Without the wait the test races the shell: signalling before `trap`
        // has run finds SIGTERM at its default disposition and the process dies, so
        // the ladder reports an ordinary stop and the assertion fails for a reason
        // that has nothing to do with the ladder.
        //
        // Production cannot have this race — `start_browser` waits for the head's
        // readiness line before anything could stop it — so waiting here is
        // matching production rather than papering over a real hazard. It is also
        // the same discipline: a process says when it is ready, rather than the
        // caller guessing with a sleep.
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; echo ready; exec sleep 60"]);
        command.stdout(Stdio::piped());
        own_process_group(&mut command);
        let mut child = command.spawn().expect("spawn a SIGTERM-deaf process");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read the fixture's readiness line");
        assert_eq!(line.trim(), "ready");
        child
    }

    #[cfg(not(unix))]
    fn deaf_to_term() -> Child {
        // No ask is available on this platform, so every stop is already a force —
        // the assertion holds for a different reason, which is worth stating rather
        // than skipping.
        long_lived()
    }

    /// Asking reaches a World's own children, not just the process we spawned.
    ///
    /// The property the group buys, and the one that matters for a World that runs
    /// a server rather than serving a page: `Child::kill` signals one process and
    /// leaves descendants (rust-lang/rust#115241), so a supervisor addressing the
    /// process alone would stop the parent and leave the work running.
    ///
    /// The fixture is a shell waiting on a child. Only the child is reachable by a
    /// signal that ignores the group — the shell here ignores SIGTERM — so if the
    /// tree goes down inside the budget, the signal reached the child.
    #[cfg(unix)]
    #[test]
    fn asking_reaches_the_childs_children() {
        // Readiness is printed *after* the child is forked, so reading it proves
        // there is a descendant to reach. Spawning and asking straight away
        // raced the shell: a group TERM arriving after `trap` but before the
        // fork was ignored by the shell and missed the child entirely, and the
        // stop escalated to a force — failing on this assertion, under load,
        // for a reason that is not the property. `deaf_to_term` below already
        // waits for a readiness line; this one did not.
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; sleep 60 & echo ready; wait"]);
        command.stdout(Stdio::piped());
        own_process_group(&mut command);
        let mut child = command.spawn().expect("spawn a parent with a child");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read the fixture's readiness line");
        assert_eq!(line.trim(), "ready", "the child must exist before we ask");

        let mut head = OwnedHead {
            facts: HeadFacts {
                id: "tree".into(),
                kind: HeadKind::Browser,
                device: None,
                orbit: None,
                world: None,
                identity: "test".into(),
                ownership: Ownership::Owned,
                pid: None,
                url: None,
                state: HeadState::Running,
            },
            child,
        };

        assert_eq!(
            head.stop_within(Duration::from_secs(2))
                .expect("stop a tree"),
            Stopped::Stopped,
            "the ask must reach a descendant; a process-only signal would have \
             left the child running and forced the parent"
        );
    }

    /// The three states are three, and uncertainty is not usability.
    ///
    /// `Unknown` resolving to unusable is the direction that matters: reporting a
    /// head usable when nobody could check hands somebody a URL for a process that
    /// may not be there.
    #[test]
    fn uncertainty_is_not_usable_and_an_mcp_head_is_never_claimed_running() {
        assert!(HeadState::Running.usable());
        assert!(!HeadState::Exited {
            status: "exit status: 0".into()
        }
        .usable());
        assert!(!HeadState::Unknown {
            why: "no handle".into()
        }
        .usable());

        // An MCP head is spawned by the agent's harness, so this process holds no
        // handle. Claiming `Running` there would be a statement about a child it
        // never had.
        let mcp = mcp_head(
            "mcp".into(),
            None,
            std::path::PathBuf::from("/identity"),
            None,
        );
        match mcp.state {
            HeadState::Unknown { why } => assert!(
                why.contains("holds no handle"),
                "the reason must name why it cannot be known: {why}"
            ),
            other => panic!("an unowned head claimed {other:?}"),
        }
    }

    /// A process that exits immediately, for the dead-handle cases.
    fn short_lived() -> Child {
        spawned(if cfg!(windows) {
            ("cmd", vec!["/C", "exit 0"])
        } else {
            ("true", vec![])
        })
    }

    /// A process that will outlive the test unless stopped.
    fn long_lived() -> Child {
        spawned(if cfg!(windows) {
            ("cmd", vec!["/C", "ping -n 60 127.0.0.1 >NUL"])
        } else {
            ("sleep", vec!["60"])
        })
    }

    /// Spawned the way `start_browser` spawns, group and all.
    ///
    /// The group is not incidental to these tests. A first draft omitted it and
    /// `stop` reported `Forced` on a healthy process in nineteen milliseconds:
    /// `kill(-pid, …)` addresses a *group*, so with no group there was nothing to
    /// signal and the ladder fell through to force. A helper that spawns
    /// differently from production would have hidden the coupling instead of
    /// proving it.
    fn spawned((program, args): (&str, Vec<&str>)) -> Child {
        let mut command = Command::new(program);
        command.args(args);
        own_process_group(&mut command);
        command.spawn().expect("spawn a test process")
    }

    /// The readiness line is the contract between a head and whatever started
    /// it. Anything else is a head that came up wrong, and saying so beats
    /// handing back a `HeadFacts` with no address in it.
    #[test]
    fn only_a_well_formed_readiness_line_is_accepted() {
        assert!(serde_json::from_str::<Ready>(
            r#"{"url":"http://127.0.0.1:1/?token=a","token":"a","port":1}"#
        )
        .is_ok());
        for wrong in [
            r#"{"url":"http://127.0.0.1:1/"}"#,
            r#"listening on 7717"#,
            r#"{}"#,
        ] {
            assert!(
                serde_json::from_str::<Ready>(wrong).is_err(),
                "'{wrong}' was accepted as a readiness line"
            );
        }
    }
}
