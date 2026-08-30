//! `lait install`: the proven binary, the record beside it, and the unit.
//!
//! The binary a person untarred and ran is a bootstrapper, never what gets
//! installed. It resolves the channel through the same signed chain the
//! daemon updates by — pointer, manifest, size, digest — and installs *those*
//! bytes under a root the daemon will own, with a systemd unit that says
//! `LAIT_SUPERVISED=1` so a later swap is an exit and `Restart=` is the
//! spawner. Installing itself instead would put whatever a mirror served, or
//! whatever a stale doc line named, on the box for good: "could not verify"
//! must never degrade into "installed anyway".
//!
//! Planning and applying are separate so every failure short of the swap
//! leaves the machine as it was: [`plan`] asks the feed and proves the bytes;
//! [`apply`] writes. The decisions are pure and injected the way
//! `feed::resolve_with` and `stage_with` are, which is what lets a macOS test
//! prove what a Linux box will do.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::feed::{self, Channel};
use super::watch::SERVICE_INSTALLED_FILE;

/// The unit as shipped, before its paths are filled in.
const UNIT_TEMPLATE: &str = include_str!("../../packaging/linux/lait.service.in");

/// The unit's name in both managers.
pub const UNIT_NAME: &str = "lait.service";

/// The system account the daemon runs as, and the root it owns.
#[cfg(target_os = "linux")]
const SERVICE_USER: &str = "lait";
const SYSTEM_ROOT: &str = "/var/lib/lait";
const SYSTEM_UNIT_DIR: &str = "/etc/systemd/system";

/// How long [`tail`] waits for the freshly started daemon to answer.
const TAIL_PATIENCE: Duration = Duration::from_secs(20);

/// The record beside the binary — what makes `<root>/bin/lait` an
/// installation rather than a directory somebody called `bin`, and what a
/// second install line reads to refuse crossing a system install with a user
/// one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub version: String,
    pub target: String,
    pub channel: String,
    /// `lait.service` for a system install, `user:lait.service` for `--user`.
    pub unit: String,
    pub installed_at: u64,
}

impl Installed {
    fn path(root: &Path) -> PathBuf {
        root.join("bin").join(SERVICE_INSTALLED_FILE)
    }

    fn read(root: &Path) -> Option<Self> {
        let bytes = std::fs::read(Self::path(root)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// Everything an install is going to do, decided and proven before any of it
/// is written.
#[derive(Debug)]
pub struct Plan {
    pub root: PathBuf,
    pub unit_path: PathBuf,
    /// The unit text, rendered for `root`.
    pub unit: String,
    pub user: bool,
    pub channel: Channel,
    pub resolved: feed::Resolved,
    /// The proven bytes out of the channel's archive — never this executable.
    pub binary: Vec<u8>,
    pub version: semver::Version,
    /// The unit is already active: this is a re-run, and the daemon is
    /// restarted onto the new binary rather than enabled a second time.
    pub restart: bool,
}

/// The facts about this machine an install decides on, read once.
///
/// A struct rather than three calls inside [`plan_with`] so the decision can
/// be driven against a stated host: "not root" and "the unit is active" are
/// answers a test states, not ones it can arrange.
#[derive(Debug, Clone)]
pub struct Host {
    /// The effective user is root.
    pub root_user: bool,
    /// The invoking user's home, which a `--user` layout is placed under.
    pub home: Option<PathBuf>,
    /// `lait.service` is active under the manager this install targets.
    pub unit_active: bool,
}

impl Host {
    fn observe(user: bool) -> Self {
        Self {
            root_user: effective_root(),
            home: directories::BaseDirs::new().map(|base| base.home_dir().to_path_buf()),
            unit_active: unit_active(user),
        }
    }
}

#[cfg(unix)]
fn effective_root() -> bool {
    // SAFETY: geteuid takes nothing, touches nothing, and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn effective_root() -> bool {
    false
}

/// The unit's label — what the record carries and the install line prints —
/// which is how a system install and a user one are told apart at the next
/// install line.
pub fn unit_label(user: bool) -> String {
    if user {
        format!("user:{UNIT_NAME}")
    } else {
        UNIT_NAME.to_string()
    }
}

/// Where the layout goes when nobody said: `/var/lib/lait` for the system,
/// `~/.local/share/lait` for a person.
fn default_root(user: bool, home: Option<&Path>) -> Result<PathBuf> {
    if !user {
        return Ok(PathBuf::from(SYSTEM_ROOT));
    }
    home.map(|home| home.join(".local").join("share").join("lait"))
        .ok_or_else(|| anyhow!("--user needs a home directory to install under, and none is known"))
}

/// Where the unit goes: the system manager's directory, or the person's.
fn unit_path(user: bool, home: Option<&Path>) -> Result<PathBuf> {
    if !user {
        return Ok(Path::new(SYSTEM_UNIT_DIR).join(UNIT_NAME));
    }
    home.map(|home| {
        home.join(".config")
            .join("systemd")
            .join("user")
            .join(UNIT_NAME)
    })
    .ok_or_else(|| {
        anyhow!("--user needs a home directory to put the unit under, and none is known")
    })
}

/// The unit text for `root`, from the template in `packaging/linux`.
///
/// A `--user` unit loses the four lines a user manager cannot honour —
/// `User=`, `Group=`, `ProtectSystem=`, `ReadWritePaths=` — and is wanted by
/// `default.target`, because `multi-user.target` does not exist in a user
/// manager and a unit enabled into it is one that never starts.
pub fn render_unit(root: &Path, user: bool, displays: bool) -> String {
    let display_line = if displays { "" } else { " LAIT_DISPLAY=off" };
    let filled = UNIT_TEMPLATE
        .replace("@ROOT@", &root.display().to_string())
        .replace("@DISPLAY_LINE@", display_line);
    let mut unit = String::with_capacity(filled.len());
    for line in filled.lines() {
        if line.starts_with('#') {
            continue;
        }
        if user {
            if ["User=", "Group=", "ProtectSystem=", "ReadWritePaths="]
                .iter()
                .any(|key| line.starts_with(key))
            {
                continue;
            }
            if line == "WantedBy=multi-user.target" {
                unit.push_str("WantedBy=default.target\n");
                continue;
            }
        }
        unit.push_str(line);
        unit.push('\n');
    }
    unit
}

/// Decide the whole install against the real feed and this machine.
///
/// `LAIT_CONFIG_ROOT` is set to the root for the rest of the process so the
/// feed's freshness ratchet lands under it — the daemon that starts next
/// reads the same file, and a ratchet written to the installer's own config
/// root would leave the service accepting a pointer this install already
/// saw.
pub fn plan(
    channel: Option<Channel>,
    user: bool,
    displays: bool,
    root: Option<PathBuf>,
) -> Result<Plan> {
    if cfg!(not(target_os = "linux")) {
        bail!("lait install writes a systemd unit, and this is not a Linux host");
    }
    let host = Host::observe(user);
    let root = match root {
        Some(root) => root,
        None => default_root(user, host.home.as_deref())?,
    };
    std::env::set_var("LAIT_CONFIG_ROOT", &root);
    let channel = channel.unwrap_or_else(Channel::current);
    plan_with(
        || feed::resolve(channel),
        feed::http_fetch,
        env!("LAIT_TARGET"),
        &host,
        channel,
        user,
        displays,
        root,
    )
}

/// [`plan`] with the feed and the host injected.
///
/// The "current" version handed to the stager is `0.0.0`, so the channel's
/// release is always newer than what is running: the bytes that come back are
/// the archive's, whatever binary this is.
#[allow(clippy::too_many_arguments)]
pub fn plan_with<R, F>(
    resolve: R,
    fetch: F,
    target: &str,
    host: &Host,
    channel: Channel,
    user: bool,
    displays: bool,
    root: PathBuf,
) -> Result<Plan>
where
    R: FnOnce() -> std::result::Result<feed::Resolved, feed::Failure>,
    F: Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure>,
{
    if !user && !host.root_user {
        bail!(
            "lait install writes {SYSTEM_ROOT} and a system unit; run it as root, \
             or pass --user for an install under your own home"
        );
    }
    let label = unit_label(user);
    if let Some(existing) = Installed::read(&root) {
        if existing.unit != label {
            bail!(
                "{} already holds lait {} installed as {}; this line would install it as {label}. \
                 Re-run it the way it was installed, or pick another --root",
                root.display(),
                existing.version,
                existing.unit
            );
        }
    }
    let resolved = resolve().map_err(|error| anyhow!("{error}"))?;
    let binary = super::stage_with(fetch, &resolved, &semver::Version::new(0, 0, 0), target)
        .map_err(|error| anyhow!("{error}"))?
        .ok_or_else(|| {
            anyhow!(
                "the {} channel points at {}, which offers nothing to install",
                channel.as_str(),
                resolved.version
            )
        })?;
    let unit_path = unit_path(user, host.home.as_deref())?;
    Ok(Plan {
        unit: render_unit(&root, user, displays),
        unit_path,
        user,
        channel,
        binary,
        version: resolved.version.clone(),
        resolved,
        restart: host.unit_active,
        root,
    })
}

/// Write the plan and hand the result to the manager.
///
/// Order matters twice. The service account exists before the root does so
/// the tree can be handed to it whole; and it is handed over *after* every
/// file is written, because the feed's ratchet and the channel record were
/// written by root at plan time and a daemon that cannot advance its own
/// ratchet has no replay protection while looking like one that does.
pub fn apply(plan: &Plan) -> Result<()> {
    if !plan.user {
        ensure_service_user(&plan.root)?;
    }
    write_layout(plan)?;
    if !plan.user {
        own_tree(&plan.root)?;
    }
    systemctl(plan.user, &["daemon-reload"])?;
    if plan.restart {
        systemctl(plan.user, &["restart", UNIT_NAME])
    } else {
        systemctl(plan.user, &["enable", "--now", UNIT_NAME])
    }
}

/// The files, and only the files: the binary in one rename, its record, the
/// channel of record, and the unit. What this writes is exactly the shape
/// `Installation::of` recognises as a service — proven by a test rather than
/// by two modules agreeing in prose.
pub fn write_layout(plan: &Plan) -> Result<()> {
    let bin = plan.root.join("bin");
    std::fs::create_dir_all(&bin).with_context(|| format!("create {}", bin.display()))?;
    let installed = bin.join("lait");
    let staged = bin.join("lait.tmp");
    std::fs::write(&staged, &plan.binary).with_context(|| format!("write {}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .context("mark the binary executable")?;
    }
    if let Err(error) = std::fs::rename(&staged, &installed) {
        let _ = std::fs::remove_file(&staged);
        return Err(error).with_context(|| format!("move the binary to {}", installed.display()));
    }
    let record = Installed {
        version: plan.version.to_string(),
        target: env!("LAIT_TARGET").to_string(),
        channel: plan.channel.as_str().to_string(),
        unit: unit_label(plan.user),
        installed_at: super::watch::now(),
    };
    std::fs::write(
        Installed::path(&plan.root),
        serde_json::to_vec_pretty(&record).context("encode the install record")?,
    )
    .context("write the install record")?;
    plan.channel
        .record_at(&plan.root)
        .context("record the channel")?;
    if let Some(dir) = plan.unit_path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(&plan.unit_path, &plan.unit)
        .with_context(|| format!("write {}", plan.unit_path.display()))?;
    Ok(())
}

/// What the daemon under the fresh unit has to say for itself.
///
/// Three answers that are never folded: a code to carry to Astrolabe, a
/// device that is already somebody's, and a daemon that is up and has minted
/// nothing yet — the last is the truthful one until the pairing surface
/// lands, and it is not "already paired".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tail {
    Code {
        code: String,
        direct: Vec<std::net::SocketAddr>,
    },
    AlreadyPaired {
        devices: usize,
    },
    NoCodeYet,
}

impl std::fmt::Display for Tail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code { code, .. } => {
                write!(f, "Pair it: enter {code} in Astrolabe → Devices")
            }
            Self::AlreadyPaired { devices } => write!(f, "Already paired ({devices} devices)"),
            Self::NoCodeYet => write!(f, "Daemon up, no code yet — systemctl status lait"),
        }
    }
}

/// What a `HostContext` answer says about pairing. Orientation carries no
/// code and no device set yet, so a daemon that answers at all is
/// [`Tail::NoCodeYet`]; anything else is not an answer.
fn read_context(response: &crate::control::Response) -> Option<Tail> {
    use crate::control::{HostReply, Response};
    match response {
        Response::Host(HostReply::Context { .. }) => Some(Tail::NoCodeYet),
        _ => None,
    }
}

/// Ask the daemon under `root` for its pairing state, for up to twenty
/// seconds after the manager started it.
///
/// Over the control socket, which is unauthenticated locally, and without
/// ever spawning: the manager owns the process now, and a daemon this
/// installer started would be one `systemctl stop` cannot reach. A daemon
/// that never answers is an error naming where to look, not one of the
/// three answers — "could not be asked" is not "no code yet".
pub async fn tail(root: &Path) -> Result<Tail> {
    use crate::control::{ClientRequest, ControlRoute, Request};
    let home = root.join("daemon");
    let envelope = ClientRequest::routed(Request::HostContext, ControlRoute::Daemon, None);
    let ask = async {
        loop {
            if let Ok(response) = crate::control::send(&home, &envelope).await {
                if let Some(tail) = read_context(&response) {
                    return tail;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    tokio::time::timeout(TAIL_PATIENCE, ask).await.map_err(|_| {
        anyhow!(
            "the daemon did not answer within {}s — journalctl -u lait",
            TAIL_PATIENCE.as_secs()
        )
    })
}

#[cfg(target_os = "linux")]
fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("run {program}"))?;
    if !status.success() {
        bail!("{program} {} exited with {status}", args.join(" "));
    }
    Ok(())
}

/// `systemctl`, against the manager this install targets.
#[cfg(target_os = "linux")]
fn systemctl(user: bool, args: &[&str]) -> Result<()> {
    let mut argv: Vec<&str> = Vec::with_capacity(args.len().saturating_add(1));
    if user {
        argv.push("--user");
    }
    argv.extend_from_slice(args);
    run("systemctl", &argv)
}

#[cfg(target_os = "linux")]
fn unit_active(user: bool) -> bool {
    let mut argv = vec![];
    if user {
        argv.push("--user");
    }
    argv.extend_from_slice(&["is-active", "--quiet", UNIT_NAME]);
    std::process::Command::new("systemctl")
        .args(&argv)
        .status()
        .is_ok_and(|status| status.success())
}

/// The system account, created if it is not there: no login, its home the
/// root it will own.
#[cfg(target_os = "linux")]
fn ensure_service_user(root: &Path) -> Result<()> {
    let exists = std::process::Command::new("id")
        .args(["-u", SERVICE_USER])
        .output()
        .is_ok_and(|output| output.status.success());
    if exists {
        return Ok(());
    }
    run(
        "useradd",
        &[
            "--system",
            "--home-dir",
            &root.display().to_string(),
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            SERVICE_USER,
        ],
    )
}

#[cfg(target_os = "linux")]
fn own_tree(root: &Path) -> Result<()> {
    run(
        "chown",
        &[
            "-R",
            &format!("{SERVICE_USER}:{SERVICE_USER}"),
            &root.display().to_string(),
        ],
    )
}

#[cfg(not(target_os = "linux"))]
fn systemctl(_user: bool, _args: &[&str]) -> Result<()> {
    bail!("there is no systemd on this host")
}

#[cfg(not(target_os = "linux"))]
fn unit_active(_user: bool) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn ensure_service_user(_root: &Path) -> Result<()> {
    bail!("there is no system account to create on this host")
}

#[cfg(not(target_os = "linux"))]
fn own_tree(_root: &Path) -> Result<()> {
    bail!("there is no service account to hand the tree to on this host")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::tests::{sealed_feed, windows_release_zip};

    fn host(root_user: bool, home: &Path, unit_active: bool) -> Host {
        Host {
            root_user,
            home: Some(home.to_path_buf()),
            unit_active,
        }
    }

    /// A sealed feed offering `binary` at 0.9.0, and the injected resolve and
    /// fetch that read it.
    fn feed_offering(
        binary: &[u8],
    ) -> (
        std::collections::HashMap<String, Vec<u8>>,
        [u8; 32],
        &'static str,
    ) {
        let url = "https://feed.example/releases/0.9.0/lait-x86_64-pc-windows-msvc.zip";
        let archive = windows_release_zip(binary);
        let digest = blake3::hash(&archive).to_hex().to_string();
        let (objects, pubkey) = sealed_feed("0.9.0", url, &archive, archive.len() as u64, &digest);
        (objects, pubkey, "x86_64-pc-windows-msvc")
    }

    fn plan_against(
        objects: &std::collections::HashMap<String, Vec<u8>>,
        pubkey: [u8; 32],
        target: &str,
        host: &Host,
        user: bool,
        root: &Path,
    ) -> Result<Plan> {
        let fetch = |u: &str| {
            objects
                .get(u)
                .cloned()
                .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
        };
        plan_with(
            || {
                feed::resolve_with(
                    fetch,
                    Channel::Test,
                    "https://feed.example",
                    &[pubkey],
                    None,
                )
            },
            |u, _| fetch(u),
            target,
            host,
            Channel::Test,
            user,
            false,
            root.to_path_buf(),
        )
    }

    /// The unit is the template in `packaging/linux` with the paths filled
    /// in, and the lines that matter are the ones the design pins: the
    /// supervisor contract, the restart bound, no display bind unless asked,
    /// and nothing that lands with a later slice.
    #[test]
    fn the_unit_is_the_one_in_packaging_with_the_paths_filled_in() {
        let root = Path::new("/var/lib/lait");
        let unit = render_unit(root, false, false);
        assert_eq!(
            unit,
            render_unit(root, false, false),
            "rendering is not deterministic"
        );
        for line in [
            "ExecStart=/var/lib/lait/bin/lait daemon",
            "Environment=LAIT_CONFIG_ROOT=/var/lib/lait HOME=/var/lib/lait LAIT_SUPERVISED=1 LAIT_DISPLAY=off",
            "Restart=always",
            "StartLimitBurst=5",
            "User=lait",
            "ReadWritePaths=/var/lib/lait",
            "WantedBy=multi-user.target",
        ] {
            assert!(unit.lines().any(|l| l == line), "missing {line:?} in:\n{unit}");
        }
        assert!(!unit.contains("KillMode"), "KillMode must stay the default");
        assert!(
            !unit.contains("AmbientCapabilities"),
            "capabilities land with the net plane, not before something uses them"
        );
        assert!(
            !unit.contains('@'),
            "an unfilled placeholder survived:\n{unit}"
        );
        assert!(
            !unit.contains('#'),
            "template commentary leaked into the unit"
        );

        let displays = render_unit(root, false, true);
        assert!(
            !displays.contains("LAIT_DISPLAY"),
            "--displays must leave the coordinator on"
        );
        assert!(displays.contains("LAIT_SUPERVISED=1"));

        let user = render_unit(Path::new("/home/p/.local/share/lait"), true, false);
        for dropped in ["User=", "Group=", "ProtectSystem=", "ReadWritePaths="] {
            assert!(
                !user.contains(dropped),
                "a user manager cannot honour {dropped}:\n{user}"
            );
        }
        assert!(user.contains("WantedBy=default.target"));
        assert!(user.contains("ExecStart=/home/p/.local/share/lait/bin/lait daemon"));
        assert!(user.contains("LAIT_SUPERVISED=1 LAIT_DISPLAY=off"));
    }

    /// The whole point of the mode: what a plan installs is what the channel
    /// proves, never the bootstrapper that is running.
    #[test]
    fn the_plan_never_installs_the_bytes_it_is_running() {
        let binary = b"lait 0.9.0 as the channel proves it";
        let (objects, pubkey, target) = feed_offering(binary);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let host = host(true, dir.path(), true);

        let plan = plan_against(&objects, pubkey, target, &host, false, &root)
            .expect("a root install against a sealed feed plans");
        assert_eq!(
            plan.binary, binary,
            "the plan must carry the archive's bytes"
        );
        assert_ne!(
            plan.binary,
            std::fs::read(std::env::current_exe().unwrap()).unwrap(),
            "the plan carries the executable that is running"
        );
        assert_eq!(plan.version, semver::Version::new(0, 9, 0));
        assert!(
            plan.unit
                .contains(&format!("ExecStart={}/bin/lait daemon", root.display())),
            "the unit is not rendered for the root the plan installs under"
        );
        assert_eq!(
            plan.unit_path,
            Path::new("/etc/systemd/system/lait.service")
        );
        assert!(
            plan.restart,
            "an active unit is restarted, not enabled twice"
        );
        assert_eq!(plan.root, root);
    }

    /// The two refusals a plan makes before it asks the feed: a system
    /// install by somebody who is not root, and an install line crossing a
    /// root that was installed the other way.
    #[test]
    fn a_plan_refuses_a_non_root_system_install_and_a_root_another_unit_holds() {
        let (objects, pubkey, target) = feed_offering(b"whatever");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");

        let error = plan_against(
            &objects,
            pubkey,
            target,
            &host(false, dir.path(), false),
            false,
            &root,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("--user"),
            "the refusal must name the way out: {error}"
        );

        let user = plan_against(
            &objects,
            pubkey,
            target,
            &host(false, dir.path(), false),
            true,
            &root,
        )
        .expect("a --user install needs no root");
        assert!(
            !user.unit.contains("User="),
            "a user plan carries a system unit"
        );
        assert_eq!(
            user.unit_path,
            dir.path().join(".config/systemd/user/lait.service")
        );
        assert!(!user.restart);

        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(
            Installed::path(&root),
            serde_json::to_vec(&Installed {
                version: "0.8.0".into(),
                target: target.into(),
                channel: "stable".into(),
                unit: "user:lait.service".into(),
                installed_at: 0,
            })
            .unwrap(),
        )
        .unwrap();
        let error = plan_against(
            &objects,
            pubkey,
            target,
            &host(true, dir.path(), false),
            false,
            &root,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("user:lait.service"),
            "crossing a user install with a system one must be refused by name: {error}"
        );
    }

    /// What the install line writes is the shape the daemon's watcher
    /// recognises as a service — the composition the two modules agree on,
    /// asserted rather than described.
    #[test]
    fn the_layout_is_the_service_shape_the_watcher_recognises() {
        let binary = b"lait 0.9.0 as the channel proves it";
        let (objects, pubkey, target) = feed_offering(binary);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let mut plan = plan_against(
            &objects,
            pubkey,
            target,
            &host(false, dir.path(), false),
            true,
            &root,
        )
        .unwrap();
        plan.unit_path = dir.path().join("units").join(UNIT_NAME);

        write_layout(&plan).expect("the layout writes into a scratch root");

        let lait = root.join("bin").join("lait");
        assert_eq!(std::fs::read(&lait).unwrap(), binary);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&lait).unwrap().permissions().mode() & 0o111,
                0o111,
                "the binary must be executable"
            );
        }
        assert!(
            !root.join("bin").join("lait.tmp").exists(),
            "the staging name survived the rename"
        );
        let record = Installed::read(&root).expect("the record beside the binary");
        assert_eq!(record.version, "0.9.0");
        assert_eq!(record.channel, "test");
        assert_eq!(record.unit, "user:lait.service");
        assert_eq!(
            std::fs::read_to_string(root.join("update-channel")).unwrap(),
            "test",
            "the daemon under the unit must follow the channel that was installed"
        );
        assert_eq!(std::fs::read_to_string(&plan.unit_path).unwrap(), plan.unit);
        assert_eq!(
            super::super::watch::Installation::of(&lait, &root),
            Some(super::super::watch::Installation::Service { root: root.clone() }),
            "the layout the installer writes is not the one the watcher looks for"
        );
    }

    /// Orientation carries no pairing surface yet, so a daemon that answers
    /// is "no code yet" — and never "already paired", which would tell a
    /// person their new box belongs to somebody.
    #[test]
    fn a_context_answer_is_no_code_yet_and_anything_else_is_no_answer() {
        use crate::control::{HostReply, Response};
        let context = Response::Host(HostReply::Context {
            version: "0.9.0".into(),
            identity_home: "/var/lib/lait".into(),
            spaces_root: "/var/lib/lait/spaces".into(),
            worlds: vec![],
            identities: vec![],
            orbits: vec![],
            asks: vec![],
        });
        assert_eq!(read_context(&context), Some(Tail::NoCodeYet));
        assert_eq!(
            read_context(&Response::Host(HostReply::Restarting { pid: None })),
            None
        );
        assert_eq!(
            Tail::NoCodeYet.to_string(),
            "Daemon up, no code yet — systemctl status lait"
        );
    }
}
