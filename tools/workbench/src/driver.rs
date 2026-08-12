use std::path::{Path, PathBuf};

use crate::contract::DeviceFacts;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use lait::control::{ControlRoute, HostReply, OrbitAddress, Request, Response};

pub(crate) enum DaemonProbe {
    Healthy,
    Absent,
    Foreign { why: String, replaceable: bool },
}

pub(crate) trait OwnedDaemon: Send {
    fn id(&self) -> u32;
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    fn force_kill_and_wait(&mut self) -> std::io::Result<std::process::ExitStatus>;
}

struct LaitChild(lait::daemon_spawn::DaemonChild);

impl OwnedDaemon for LaitChild {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }

    fn force_kill_and_wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.force_kill_and_wait()
    }
}

#[async_trait]
pub(crate) trait DaemonDriver: Send + Sync {
    async fn probe(&self, home: &Path) -> DaemonProbe;
    async fn spawn(&self, home: &Path) -> Result<Box<dyn OwnedDaemon>>;
    async fn request_stop(&self, home: &Path) -> Result<()>;

    /// What this daemon reports about itself.
    ///
    /// `Err` means the daemon could not be asked. It is never an empty answer:
    /// the caller keeps the last good facts and marks them stale instead of
    /// replacing them with nothing.
    async fn facts(&self, _home: &Path) -> Result<DeviceFacts> {
        Ok(DeviceFacts::default())
    }

    /// Prove which process is serving `home`, or fail saying why not.
    ///
    /// The pid comes from the home's own record; the executable and start time
    /// come from the operating system. Together they are what makes a later
    /// termination safe — a pid file outlives a crashed daemon, and pids are
    /// reused, so the file alone proves nothing.
    async fn identity(&self, _home: &Path) -> Result<lait::daemon_spawn::ProcessIdentity> {
        Err(anyhow!(
            "process identity is not available from this driver"
        ))
    }

    /// Terminate a process this supervisor did not spawn, if and only if it is
    /// still exactly `expected`.
    async fn terminate_verified(
        &self,
        _expected: &lait::daemon_spawn::ProcessIdentity,
    ) -> Result<()> {
        Err(anyhow!("this driver cannot stop an unowned process"))
    }

    /// Point future spawns at a freshly staged image.
    ///
    /// Daemons already running are untouched: they hold the image they started
    /// with, which is why each device reports its own.
    async fn restage(&self, _executable: &Path) {}

    /// The peers this daemon can already see.
    ///
    /// `Ok(vec![])` is "no peers" and `Err` is "nobody could ask", and the whole
    /// point of the `Result` is that those are different answers. Collapsing
    /// them was the defect: a sampling failure rendered as a disconnection.
    async fn connections(&self, _home: &Path) -> Result<Vec<ObservedConnection>> {
        Ok(Vec::new())
    }
}

#[derive(Clone)]
pub(crate) struct ObservedConnection {
    pub(crate) space_id: String,
    pub(crate) peer_id: String,
    pub(crate) peer_nick: String,
    pub(crate) state: String,
    pub(crate) online: bool,
    pub(crate) dialable: bool,
    pub(crate) blocked_by: Option<String>,
}

pub(crate) struct LaitDriver {
    executable: PathBuf,
}

impl LaitDriver {
    pub(crate) fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    fn client(home: &Path) -> Result<lait::daemon::Client> {
        let selection = lait::config::Selection::for_identity(home);
        lait::daemon::Client::for_selection(&selection)
    }
}

#[async_trait]
impl DaemonDriver for LaitDriver {
    async fn probe(&self, home: &Path) -> DaemonProbe {
        let client = match Self::client(home) {
            Ok(client) => client,
            Err(error) => {
                return DaemonProbe::Foreign {
                    why: format!("resolve daemon home: {error:#}"),
                    replaceable: false,
                };
            }
        };
        match client.probe().await {
            lait::control::Probe::Healthy => DaemonProbe::Healthy,
            lait::control::Probe::Absent => DaemonProbe::Absent,
            lait::control::Probe::Foreign { why, replaceable } => {
                DaemonProbe::Foreign { why, replaceable }
            }
        }
    }

    async fn spawn(&self, home: &Path) -> Result<Box<dyn OwnedDaemon>> {
        if !self.executable.is_file() {
            return Err(anyhow!(
                "lait executable does not exist: {}",
                self.executable.display()
            ));
        }
        let selection = lait::config::Selection::for_identity(home);
        let daemon_home = selection.daemon_home()?;
        let log_path = lait::host_client::daemon_log_path(&daemon_home);
        let log = std::fs::File::create(&log_path)
            .with_context(|| format!("create daemon log {}", log_path.display()))?;
        let identity = selection.self_contained_home();
        let child = lait::daemon_spawn::spawn(&self.executable, Some(log), identity.as_deref())
            .with_context(|| format!("spawn {}", self.executable.display()))?;
        Ok(Box::new(LaitChild(child)))
    }

    async fn request_stop(&self, home: &Path) -> Result<()> {
        let reply = Self::client(home)?
            .request(ControlRoute::Daemon, &Request::Stop, None)
            .await?;
        match reply {
            Response::Ok { .. } => Ok(()),
            Response::Error { message, .. } => Err(anyhow!(message)),
            other => Err(anyhow!("unexpected daemon stop reply: {other:?}")),
        }
    }

    async fn identity(&self, home: &Path) -> Result<lait::daemon_spawn::ProcessIdentity> {
        let selection = lait::config::Selection::for_identity(home);
        let daemon_home = selection.daemon_home()?;
        let pid = lait::config::daemon_pid(&daemon_home).ok_or_else(|| {
            anyhow!(
                "no daemon pid recorded for {} — a daemon that predates the stamp cannot be proven",
                daemon_home.display()
            )
        })?;
        let identity = lait::daemon_spawn::identify(pid)
            .with_context(|| format!("identify daemon process {pid}"))?;
        // The image must be one this supervisor would itself have spawned.
        // Without this, any process that inherited the pid and happens to be
        // running *something* would satisfy the other three facts.
        if identity.executable != self.executable {
            return Err(anyhow!(
                "process {pid} runs {}, not the managed image {}",
                identity.executable.display(),
                self.executable.display()
            ));
        }
        Ok(identity)
    }

    async fn terminate_verified(
        &self,
        expected: &lait::daemon_spawn::ProcessIdentity,
    ) -> Result<()> {
        lait::daemon_spawn::terminate_verified(expected)
            .with_context(|| format!("stop unowned daemon {}", expected.pid))
    }

    async fn facts(&self, home: &Path) -> Result<DeviceFacts> {
        let context = Self::host_context(home).await?;
        Ok(DeviceFacts {
            version: Some(context.version),
            build: None,
            station_id: None,
            local_client_url: None,
            spaces: context
                .orbits
                .into_iter()
                .map(|orbit| orbit.space)
                .collect(),
        })
    }

    async fn connections(&self, home: &Path) -> Result<Vec<ObservedConnection>> {
        let client = Self::client(home)?;
        let context = Self::host_context(home).await?;
        let mut connections = Vec::new();
        for orbit in context.orbits {
            let Some(space) = mechanics::ids::SpaceId::parse(&orbit.space) else {
                continue;
            };
            let route = ControlRoute::Orbit {
                address: OrbitAddress::for_store(Path::new(&orbit.path), space),
            };
            // `request_if_running` is the passive half of the contract: a
            // vacant Orbit answers "not running" rather than being placed to
            // produce a peer list. An Orbit that is not up contributes no
            // connections and is not an error — listing must not cost placement.
            let peers = match client.request_if_running(route, &Request::Who).await {
                Ok(Response::Who { peers }) => peers,
                Ok(_) => continue,
                Err(error) => {
                    return Err(anyhow!("read peers for space {}: {error:#}", orbit.space));
                }
            };
            connections.extend(peers.into_iter().map(|peer| ObservedConnection {
                space_id: orbit.space.clone(),
                peer_id: peer.id,
                peer_nick: peer.nick,
                state: peer.state,
                online: peer.online,
                dialable: peer.dialable,
                blocked_by: peer.blocked_by,
            }));
        }
        Ok(connections)
    }
}

struct HostContextFacts {
    version: String,
    orbits: Vec<lait::orbits::Entry>,
}

impl LaitDriver {
    async fn host_context(home: &Path) -> Result<HostContextFacts> {
        match Self::client(home)?
            .request(ControlRoute::Daemon, &Request::HostContext, None)
            .await?
        {
            Response::Host(HostReply::Context {
                version, orbits, ..
            }) => Ok(HostContextFacts { version, orbits }),
            Response::Error { message, .. } => Err(anyhow!(message)),
            other => Err(anyhow!("unexpected host context reply: {other:?}")),
        }
    }
}
