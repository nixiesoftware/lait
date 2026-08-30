//! The host plane — the client's alone.
//!
//! Founding a Space, entering one from an invite, and signing this machine's
//! device consent all happen at the one moment when there is no Space id to put
//! in a path, and therefore when no World head exists to draw a Welcome flow.
//! That is why this cannot live in a World: the page that would host it is
//! unreachable until the thing it creates exists.

use lait::control::{
    ControlRoute, ErrorKind, HostReply, PairOffer, PairingCode, Request, Response, SponsorshipAsk,
};

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
    /// The code this device is showing while it waits to be added to a
    /// profile. `None` whenever it holds none — already added, a code spent
    /// and waiting on its confirmation, or none minted yet — which is why a
    /// surface reads this as "not yet" and never as "already added".
    pub pairing: Option<PairingCode>,
    /// Devices that answered a code entered here and are waiting on the six
    /// words being compared.
    pub pair_offers: Vec<PairOffer>,
}

/// What entering another device's code answered.
///
/// Two answers, because they end differently: one leaves a person comparing
/// six words, the other is a device that was already added and whose answer
/// had merely been lost. Folding them would make a completed pairing look
/// like a ceremony nobody finished.
///
/// The offer itself is not carried here — it is on the next reading of
/// orientation, where the surface draws it from. Only the name is, and only
/// so the sentence saying what happened can use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairEntered {
    Offered { name: String },
    AlreadyAdded,
}

/// What retiring a device cost, per Space.
///
/// Two lists rather than a count, because they are different facts: a Space
/// the device was de-listed in, and a Space where nobody could rotate the key
/// afterwards — where the device is off the list and can still read what it
/// already held. Folding the second into the first would report a fence that
/// was never raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    pub device: String,
    pub revoked_in: Vec<String>,
    pub unfenced: Vec<String>,
    /// The Spaces the removal could not be made in. Named rather than
    /// counted into the first list: those Spaces still list the retired
    /// device, which is the one outcome here worth doing something about.
    pub could_not: Vec<String>,
}

impl Retired {
    /// What happened, in one sentence a person can act on.
    ///
    /// It says "Spaces you share an actor in" because that is what was
    /// touched: a device is removed from the lists its own actor keeps, and a
    /// Space it entered as somebody else is none of this act's business.
    /// Claiming "everywhere" would be claiming more than was done.
    #[must_use]
    pub fn said(&self) -> String {
        let spaces = match self.revoked_in.len() {
            0 => "no Space of yours listed it".to_string(),
            1 => "removed from 1 Space you share an actor in".to_string(),
            n => format!("removed from {n} Spaces you share an actor in"),
        };
        let mut said = format!("retired — {spaces}");
        if !self.unfenced.is_empty() {
            said.push_str(&format!(
                "; it can still read what it already held in {} (an admin has to rotate the \
                 key there)",
                self.unfenced.join(", ")
            ));
        }
        if !self.could_not.is_empty() {
            said.push_str(&format!(
                "; {} would not take the removal and still list it — this device keeps trying",
                self.could_not.join(", ")
            ));
        }
        said
    }
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
                pairing,
                pair_offers,
            }) => Ok(HostContext {
                version,
                identity_home,
                spaces_root,
                worlds,
                identities,
                pairing,
                pair_offers,
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

    /// Add a device to this profile, from the device that already holds it:
    /// the code the new one is showing. `XXXX-XXXX`, or `XXXX-XXXX@host:port`
    /// when the two share a network and nothing relays between them.
    ///
    /// The code is passed as it was typed. Which spellings of it are the same
    /// code is the daemon's rule, and a second normalisation here would be a
    /// second rule that could disagree with it.
    pub async fn device_pair_enter(&self, code: &str) -> ClientResult<PairEntered> {
        if code.trim().is_empty() {
            return Err(ClientError::invalid("adding a device needs its code"));
        }
        let reply = self
            .host_reply(Request::DevicePairEnter {
                code: code.trim().to_owned(),
            })
            .await?;
        match reply {
            Some(HostReply::DevicePairOffer { name, .. }) => Ok(PairEntered::Offered { name }),
            Some(HostReply::DevicePaired { .. }) => Ok(PairEntered::AlreadyAdded),
            other => Err(ClientError::internal(format!(
                "unexpected answer to a device code: {other:?}"
            ))),
        }
    }

    /// Confirm an offer once the six words match, or reject it.
    ///
    /// Rejecting sends the other device nothing: an offer nobody confirmed is
    /// dropped here, and the code it came from is already spent.
    pub async fn device_pair_confirm(
        &self,
        pairing: &str,
        accept: bool,
    ) -> ClientResult<Option<String>> {
        let reply = self
            .host_reply(Request::DevicePairConfirm {
                pairing: pairing.to_owned(),
                accept,
            })
            .await?;
        // A rejection is answered as plainly as it acts: nothing was written,
        // so there is no device to name.
        if let Some(HostReply::DevicePaired { device }) = reply {
            return Ok(Some(device));
        }
        Ok(None)
    }

    /// Retire a device of this profile, from another device of it.
    ///
    /// Never asks the device first: it may be off, lost or stolen, and a
    /// retirement that needed the machine's cooperation would be one that
    /// could not be used when it is most needed. Nothing on it is deleted —
    /// what it loses is being spoken to.
    pub async fn device_retire(&self, device: &str) -> ClientResult<Retired> {
        if device.trim().is_empty() {
            return Err(ClientError::invalid("retiring a device needs its id"));
        }
        let reply = self
            .host_reply(Request::DeviceRetire {
                device: device.trim().to_owned(),
            })
            .await?;
        match reply {
            Some(HostReply::DeviceRetired {
                device,
                revoked_in,
                unfenced,
                could_not,
            }) => Ok(Retired {
                device,
                revoked_in,
                unfenced,
                could_not,
            }),
            other => Err(ClientError::internal(format!(
                "unexpected answer to a retirement: {other:?}"
            ))),
        }
    }

    /// Say whether one Space is held on one device of this profile.
    ///
    /// Excluding takes it off that machine and de-lists it there; the bytes
    /// stay where they are. Lifting the exclusion offers it again, and the
    /// device consents to it exactly as it did the first time — nothing is
    /// put back behind anybody's back.
    pub async fn replica_exclude(
        &self,
        device: &str,
        space: &str,
        excluded: bool,
    ) -> ClientResult<()> {
        if device.trim().is_empty() || space.trim().is_empty() {
            return Err(ClientError::invalid(
                "excluding a Space needs the device and the Space",
            ));
        }
        self.host_ok(Request::HostReplicaExclude {
            device: device.trim().to_owned(),
            space: space.trim().to_owned(),
            excluded,
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

    /// One host-plane request and the reply it produced, if it produced one.
    ///
    /// `None` is `Response::Ok` — accepted, with nothing to report but a
    /// sentence. Kept apart from a reply rather than mapped to a stand-in,
    /// because the callers here decide differently on each.
    async fn host_reply(&self, request: Request) -> ClientResult<Option<HostReply>> {
        let daemon = self.daemon()?;
        let reply = daemon
            .request(ControlRoute::Daemon, &request, None)
            .await
            .map_err(|error| ClientError::unreachable(format!("reach the daemon: {error:#}")))?;
        match reply {
            Response::Host(reply) => Ok(Some(reply)),
            Response::Ok { .. } => Ok(None),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected reply: {other:?}"
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
