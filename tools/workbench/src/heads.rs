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

/// A browser head this process started and holds.
pub(crate) struct OwnedHead {
    facts: HeadFacts,
    child: Child,
}

impl OwnedHead {
    pub(crate) fn facts(&self) -> &HeadFacts {
        &self.facts
    }

    /// Stop it, and collect it. Only ever called on a head we spawned — the
    /// handle *is* the proof, exactly as it is for a daemon.
    pub(crate) fn stop(mut self) -> Result<()> {
        // Already gone is a success: the caller asked for it to not be running.
        if self.child.try_wait().context("poll head")?.is_some() {
            return Ok(());
        }
        self.child.kill().context("stop head")?;
        self.child.wait().context("collect head")?;
        Ok(())
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
