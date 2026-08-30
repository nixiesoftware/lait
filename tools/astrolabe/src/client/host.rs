//! The host plane — the client's alone.
//!
//! Founding a Space, entering one from an invite, and signing this machine's
//! device consent all happen at the one moment when there is no Space id to put
//! in a path, and therefore when no World head exists to draw a Welcome flow.
//! That is why this cannot live in a World: the page that would host it is
//! unreachable until the thing it creates exists.

use lait::control::{ControlRoute, ErrorKind, HostReply, Request, Response, SponsorshipAsk};

use super::{Client, ClientError, ClientResult};

/// What this machine knows before any Space is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostContext {
    pub version: String,
    pub identity_home: String,
    /// Where to offer to put a new store when the person has no opinion.
    pub spaces_root: String,
    /// World ids this identity has selected. Not which are active in an Orbit
    /// — that is SUB-2, and conflating them is how a Library ends up listing
    /// Worlds an Orbit never activated.
    pub worlds: Vec<String>,
    pub identities: Vec<String>,
    pub orbits: Vec<OrbitEntry>,
    /// Unsponsored agents waiting on this identity to sponsor them.
    ///
    /// Host-plane state. The client diffs this list the same way it diffs a
    /// workbench snapshot: a new row is an interruption, a standing row is
    /// not, and the first reading of a machine that already has asks *is*
    /// news — a decision is waiting, unlike four peers who were already here.
    pub asks: Vec<SponsorshipAsk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbitEntry {
    pub space: String,
    /// Advisory, and may lag a rename: the display name is owned by a World
    /// today, which is SUB-1. Carried as what it is rather than as truth.
    pub name: String,
    pub path: String,
    pub last_opened: u64,
}

impl Client {
    /// Orientation: what host release is running, which identities exist,
    /// which Worlds are selected, and which Orbits this identity has.
    pub async fn host_context(&self) -> ClientResult<HostContext> {
        let daemon = self.daemon()?;
        let reply = daemon
            .request(ControlRoute::Daemon, &Request::HostContext, None)
            .await
            .map_err(|error| ClientError::unreachable(format!("{error:#}")))?;
        match reply {
            Response::Host(HostReply::Context {
                version,
                identity_home,
                spaces_root,
                worlds,
                identities,
                orbits,
                asks,
                // The client's Devices surface (S2-4) maps these; until then
                // they are named so the destructure stays exhaustive.
                pairing: _,
                pair_offers: _,
            }) => Ok(HostContext {
                version,
                identity_home,
                spaces_root,
                worlds,
                identities,
                orbits: orbits
                    .into_iter()
                    .map(|orbit| OrbitEntry {
                        space: orbit.space,
                        name: orbit.name,
                        path: orbit.path,
                        last_opened: orbit.last_opened,
                    })
                    .collect(),
                asks,
            }),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected host context reply: {other:?}"
            ))),
        }
    }

    /// Found a new Space — in `home` if one is named, else where the daemon
    /// places it (under its spaces root, by the Space's id).
    ///
    /// Nothing is created implicitly. A person with a fresh install has no
    /// Space and no World head, and this is the call that ends that — which is
    /// exactly why it cannot be a page. Placement is not the person's question:
    /// a store's path is a fact the daemon reports afterwards, not an input a
    /// founding form has to extract from somebody.
    pub async fn space_found(
        &self,
        home: Option<&str>,
        name: &str,
        nick: Option<String>,
    ) -> ClientResult<()> {
        if name.trim().is_empty() {
            return Err(ClientError::invalid("a new Space needs a name"));
        }
        self.host_ok(Request::HostSpaceFound {
            home: home.map(str::to_owned),
            name: name.to_owned(),
            nick,
        })
        .await
    }

    /// Enter an existing Space from an invite link.
    ///
    /// The acceptance shape for a fresh install: an invite in hand must reach a
    /// converged Space *without opening a browser*.
    pub async fn space_enter(
        &self,
        link: &str,
        home: Option<&str>,
        nick: Option<String>,
    ) -> ClientResult<()> {
        if link.trim().is_empty() {
            return Err(ClientError::invalid("entering a Space needs an invite"));
        }
        self.host_ok(Request::HostSpaceEnter {
            link: link.trim().to_owned(),
            home: home.map(str::to_owned),
            nick,
        })
        .await
    }

    /// Sign this machine's consent to join an existing actor.
    ///
    /// The one host request that touches no store: the machine running it has
    /// no membership anywhere yet, which is the whole point of enrolment.
    pub async fn device_consent(&self, token: &str) -> ClientResult<()> {
        if token.trim().is_empty() {
            return Err(ClientError::invalid("device consent needs an invite token"));
        }
        self.host_ok(Request::HostDeviceConsent {
            token: token.trim().to_owned(),
        })
        .await
    }

    /// Forget an Orbit's registration without touching what is on disk.
    ///
    /// Forgetting and deleting are separate here for the same reason they are
    /// separate for devices: one is a registry edit and the other destroys
    /// data, and a single verb that did both would make the safe operation
    /// carry the dangerous one's risk.
    pub async fn orbit_forget(&self, space: &str) -> ClientResult<()> {
        self.host_ok(Request::HostOrbitForget {
            selector: space.to_owned(),
        })
        .await
    }

    /// Re-derive the Orbit registry from what is actually on disk.
    pub async fn orbit_rebuild(&self, orbit: &str) -> ClientResult<()> {
        self.host_ok(Request::HostOrbitRebuild {
            orbit: orbit.to_owned(),
        })
        .await
    }

    /// Durably consent to one native World update. The returned operation was
    /// persisted before the daemon began any network or migration work.
    pub async fn world_update(&self, world: &str) -> ClientResult<lait::update::consent::Job> {
        let daemon = self.daemon()?;
        let reply = daemon
            .request(
                ControlRoute::Daemon,
                &Request::HostWorldUpdate {
                    world: world.to_owned(),
                },
                None,
            )
            .await
            .map_err(|error| ClientError::unreachable(format!("reach the daemon: {error:#}")))?;
        match reply {
            Response::Host(HostReply::WorldUpdate { job: Some(job), .. }) => Ok(job),
            Response::Error {
                message,
                error_kind: ErrorKind::Busy | ErrorKind::Capacity,
            } => Err(ClientError::unreachable(message)),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected World update reply: {other:?}"
            ))),
        }
    }

    pub async fn world_update_status(
        &self,
        world: &str,
    ) -> ClientResult<Option<lait::update::consent::Job>> {
        let daemon = self.daemon()?;
        let reply = daemon
            .request(
                ControlRoute::Daemon,
                &Request::HostWorldUpdateStatus {
                    world: world.to_owned(),
                },
                None,
            )
            .await
            .map_err(|error| ClientError::unreachable(format!("reach the daemon: {error:#}")))?;
        match reply {
            Response::Host(HostReply::WorldUpdate { job, .. }) => Ok(job),
            Response::Error {
                message,
                error_kind: ErrorKind::Busy | ErrorKind::Capacity,
            } => Err(ClientError::unreachable(message)),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected World update status reply: {other:?}"
            ))),
        }
    }

    async fn host_ok(&self, request: Request) -> ClientResult<()> {
        let daemon = self.daemon()?;
        let reply = daemon
            .request(ControlRoute::Daemon, &request, None)
            .await
            .map_err(|error| ClientError::unreachable(format!("reach the daemon: {error:#}")))?;
        match reply {
            Response::Ok { .. } | Response::Host(_) => Ok(()),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected reply: {other:?}"
            ))),
        }
    }
}
