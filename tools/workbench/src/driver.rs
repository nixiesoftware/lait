use std::path::{Path, PathBuf};

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

    async fn connections(&self, _home: &Path) -> Vec<ObservedConnection> {
        Vec::new()
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

    async fn connections(&self, home: &Path) -> Vec<ObservedConnection> {
        let Ok(client) = Self::client(home) else {
            return Vec::new();
        };
        let Ok(Response::Host(HostReply::Context { orbits, .. })) = client
            .request(ControlRoute::Daemon, &Request::HostContext, None)
            .await
        else {
            return Vec::new();
        };
        let mut connections = Vec::new();
        for orbit in orbits {
            let Some(space) = mechanics::ids::SpaceId::parse(&orbit.space) else {
                continue;
            };
            let route = ControlRoute::Orbit {
                address: OrbitAddress::for_store(Path::new(&orbit.path), space),
            };
            let Ok(Response::Who { peers }) = client.request_if_running(route, &Request::Who).await
            else {
                continue;
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
        connections
    }
}
