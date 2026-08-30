//! Every Space a person holds appears on every device they own.
//!
//! A holder-initiated reconcile over the Own lane. For each own device that
//! is not this one, for each Space this daemon serves under its own key, if
//! the Space's ledger does not name that device: dial it, offer the Space,
//! take its consent, and add it — through the same `AddDevice` a person
//! enrols a second machine with by hand. The offered device enters the Space
//! on its own background task and converges through ordinary Contact.
//!
//! The device set only *selects* whom to offer to. It authorizes nothing:
//! the device signs its own consent, and the Space's authority re-verifies
//! that consent against my actor before anything is written. The hub has
//! already admitted every frame on the lane against the set, so nothing
//! here re-derives membership from kinship — the link says who to ask, the
//! consent says yes, the ledger says whether it happened.
//!
//! Standing is kept in memory and nowhere else. The ledger is the truth
//! about who holds a Space; what this module remembers is only whether it
//! has asked, so a restart asks again and the ledger answers "already".
//!
//! A device announces where it is on this lane, at start and whenever the
//! set changes. It has to: a daemon that restarts comes back on a new
//! ephemeral port, and under a policy with no relay or discovery the routes
//! its siblings learned once during pairing are then addresses nobody
//! answers on — every dial fails, honestly and forever. The announcement is
//! what repairs that, from whichever side is still reachable. Under
//! `Isolated` that means **at least one of the two must keep the address
//! the other knows** across the restart; nothing here remembers an address
//! across a boot, because a device that dialled remembered addresses would
//! be a second discovery mechanism, and this lane is not one.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use comms::{Incoming, Transport};
use mechanics::ids::{DeviceId, SpaceId};
use runtime::correspondence::{MAX_OWN_FRAME, OWN_ALPN};
use runtime::poison::LockRecovering;
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Semaphore};

use crate::control::{
    ControlRoute, DeviceStanding, FanoutStanding, Liveness, Request, Response, SpaceFanout,
};
use crate::daemon::correspondence::OwnDevices;
use crate::daemon::own_routes;
use crate::daemon::OrbitAddress;
use crate::orbits::{self, bootstrap, Router, StationIdentity};

/// One step per tick: the loop never dials two devices at once, so a set of
/// Spaces and devices fans out at a walking pace rather than as a burst
/// that places every Station on this daemon in the same second.
const TICK: Duration = Duration::from_secs(1);
/// How long one offer waits for its answer, dial included. A device that is
/// up answers in a round trip; one that does not answer in this long is
/// recorded as not asked, never as having declined.
const ANSWER_DEADLINE: Duration = Duration::from_secs(10);
/// Retry after a device could not be asked: thirty seconds, doubling to the
/// update watcher's period, jittered like it.
const RETRY_FLOOR: Duration = Duration::from_secs(30);
const RETRY_CEILING: Duration = Duration::from_secs(4 * 3600 + 30 * 60);
const RETRY_SPREAD: Duration = Duration::from_secs(10);
/// How long a refusal stands before the device is asked again. A refusal is
/// a condition that passes — an unplaced Station, a consent the Space would
/// not take yet — so it is remembered, not kept. A *decline* is not on this
/// clock: see [`Facts::due`].
const REFUSAL_LIFETIME: Duration = Duration::from_secs(3600);
/// How many Spaces an offered device enters at once. Each entry drives
/// admission for up to thirty seconds; a new device receiving every Space
/// must not serialize them, and must not place all of them either.
const ENTER_CONCURRENCY: usize = 2;
/// The bounded wait for this identity's routes before an offer is sent.
const ROUTES_DEADLINE: Duration = Duration::from_secs(3);

/// The wait before asking a silent device again: the floor, doubled once
/// per failure in a row, jittered like every other period in this daemon,
/// and capped so a device that is simply gone costs one dial an evening.
fn backoff(tries: u32) -> Duration {
    let base = RETRY_FLOOR
        .checked_mul(1u32 << tries.min(16))
        .unwrap_or(RETRY_CEILING)
        .min(RETRY_CEILING);
    crate::update::watch::next_delay(base, RETRY_SPREAD)
}

/// What a holder says over the Own lane. Every frame carries the sender's
/// routes, because under an isolated network the frame is the only way the
/// other side ever learns them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum OwnFrame {
    /// "You are one of my devices; here is a Space I hold and a ticket to
    /// it." `actor` is the actor the consent must name; `coordinates` is a
    /// `lait://join/…` link minted by the offering device.
    Offer {
        space: String,
        actor: String,
        coordinates: String,
        routes: Vec<SocketAddr>,
    },
    /// "This is where I am." Sent at start and on every device set change,
    /// so a device that came back on a new port is reachable again without
    /// anybody re-running the pairing.
    Hello { routes: Vec<SocketAddr> },
}

impl OwnFrame {
    /// Every frame says where its sender is, because the frame is the only
    /// thing that says so when there is no relay to ask.
    fn routes(&self) -> &[SocketAddr] {
        match self {
            Self::Offer { routes, .. } | Self::Hello { routes } => routes,
        }
    }
}

/// What the offered device answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum OwnAnswer {
    /// A `device_consent` binding for the actor and Space offered, hex.
    Consent {
        binding: String,
    },
    /// The device already holds the Space as a member.
    Held,
    Declined {
        why: String,
    },
    Refused {
        why: String,
    },
    /// A [`OwnFrame::Hello`] taken, and this device's own routes back, so
    /// one round trip repairs both directions.
    Learned {
        routes: Vec<SocketAddr>,
    },
}

/// One answer as remembered: what it was, when, and how many times in a row
/// the device could not be asked — the backoff's counter.
#[derive(Debug, Clone)]
struct Recorded {
    standing: FanoutStanding,
    at_ms: u64,
    tries: u32,
}

/// What the fan-out knows, for the reach view to render. The ledger's device
/// list per Space is kept apart from the standings on purpose: `held` and
/// `on` come from the first and are facts about a Space; a standing is only
/// this daemon's memory of asking.
#[derive(Default)]
pub(crate) struct Facts {
    standings: Mutex<BTreeMap<(DeviceId, String), Recorded>>,
    liveness: Mutex<BTreeMap<DeviceId, Liveness>>,
    ledger: Mutex<BTreeMap<String, Vec<DeviceId>>>,
}

impl Facts {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Where the last offer of `space` to `device` stood; `None` when nothing
    /// has asked yet — which is not a standing.
    #[cfg(test)]
    pub(crate) fn standing(&self, device: &DeviceId, space: &str) -> Option<FanoutStanding> {
        self.standings
            .lock_recovering()
            .get(&(device.clone(), space.to_string()))
            .map(|recorded| recorded.standing.clone())
    }

    /// What a probe of `device` last learned.
    pub(crate) fn liveness_of(&self, device: &DeviceId) -> Liveness {
        self.liveness
            .lock_recovering()
            .get(device)
            .cloned()
            .unwrap_or_default()
    }

    /// Forget what a probe last learned, because a fresher answer arrived.
    /// A dial that failed writes `CouldNotAsk`; leaving it there after the
    /// device answered would render a stale measurement as the current one,
    /// which is the same defect as calling an unmeasured device down.
    fn answered(&self, device: &DeviceId) {
        self.liveness.lock_recovering().remove(device);
    }

    /// Forget everything this daemon remembers about a device that is no
    /// longer one of the profile's. Only memory of asking goes: what a
    /// Space's ledger says is the Space's to say, and the next read of it is
    /// what corrects the rows here.
    fn forget_device(&self, device: &DeviceId) {
        self.standings
            .lock_recovering()
            .retain(|(held, _), _| held != device);
        self.liveness.lock_recovering().remove(device);
        for devices in self.ledger.lock_recovering().values_mut() {
            devices.retain(|held| held != device);
        }
    }

    /// The Spaces whose ledger, as last read here, names `device`.
    pub(crate) fn held_by(&self, device: &DeviceId) -> Vec<String> {
        self.ledger
            .lock_recovering()
            .iter()
            .filter(|(_, devices)| devices.contains(device))
            .map(|(space, _)| space.clone())
            .collect()
    }

    /// Every Space with a ledger reading or a standing, for the view.
    pub(crate) fn view(&self, own: &OwnDevices) -> Vec<SpaceFanout> {
        let ledger = self.ledger.lock_recovering().clone();
        let standings = self.standings.lock_recovering().clone();
        let mut spaces: std::collections::BTreeSet<&str> =
            ledger.keys().map(String::as_str).collect();
        spaces.extend(standings.keys().map(|(_, space)| space.as_str()));
        spaces
            .into_iter()
            .map(|space| SpaceFanout {
                space: space.to_string(),
                on: ledger
                    .get(space)
                    .map(|devices| devices.iter().map(|d| d.as_str().to_owned()).collect())
                    .unwrap_or_default(),
                standings: own
                    .devices
                    .iter()
                    .filter(|device| **device != own.me)
                    .filter_map(|device| {
                        standings
                            .get(&(device.clone(), space.to_string()))
                            .map(|recorded| DeviceStanding {
                                device: device.as_str().to_owned(),
                                standing: recorded.standing.clone(),
                            })
                    })
                    .collect(),
            })
            .collect()
    }

    fn record(&self, device: &DeviceId, space: &str, standing: FanoutStanding, now_ms: u64) {
        let mut standings = self.standings.lock_recovering();
        let key = (device.clone(), space.to_string());
        let tries = match &standing {
            FanoutStanding::CouldNotAsk { .. } => {
                standings.get(&key).map_or(1, |r| r.tries.saturating_add(1))
            }
            _ => 0,
        };
        standings.insert(
            key,
            Recorded {
                standing,
                at_ms: now_ms,
                tries,
            },
        );
    }

    /// The device just spoke to us, so whatever this daemon last recorded
    /// about reaching it was taken before that and is not current: the
    /// liveness row goes back to unmeasured, and every "could not be asked"
    /// standing for it comes due now rather than at the end of a backoff
    /// the device has already outlived.
    fn reachable_again(&self, device: &DeviceId) {
        self.liveness.lock_recovering().remove(device);
        let mut standings = self.standings.lock_recovering();
        for ((held, _), recorded) in standings.iter_mut() {
            if held != device {
                continue;
            }
            if let FanoutStanding::CouldNotAsk { why, .. } = &recorded.standing {
                recorded.standing = FanoutStanding::CouldNotAsk {
                    why: why.clone(),
                    retry_at_ms: 0,
                };
                recorded.tries = 0;
            }
        }
    }

    /// Record that `device` did not answer, with the next time to try.
    fn could_not_ask(&self, device: &DeviceId, space: &str, why: String, now_ms: u64) {
        let tries = self
            .standings
            .lock_recovering()
            .get(&(device.clone(), space.to_string()))
            .map_or(0, |r| r.tries);
        let retry_at_ms =
            now_ms.saturating_add(u64::try_from(backoff(tries).as_millis()).unwrap_or(u64::MAX));
        self.liveness
            .lock_recovering()
            .insert(device.clone(), Liveness::CouldNotAsk { why: why.clone() });
        self.record(
            device,
            space,
            FanoutStanding::CouldNotAsk { why, retry_at_ms },
            now_ms,
        );
    }

    /// Take the ledger's device list for `space` as read: every device it
    /// names is `Held`, and a device it stopped naming is forgotten so the
    /// next tick offers again — holding is the default, and the ledger is
    /// what decides it.
    fn note_ledger(
        &self,
        space: &str,
        devices: Vec<DeviceId>,
        own: Option<&OwnDevices>,
        now_ms: u64,
    ) {
        {
            let mut standings = self.standings.lock_recovering();
            standings.retain(|(device, s), recorded| {
                s != space
                    || devices.contains(device)
                    || !matches!(recorded.standing, FanoutStanding::Held)
            });
        }
        // A standing is a record of asking, and this daemon asks its own
        // devices and never itself: the ledger names both, and a row for
        // either would be a row nothing on this side ever wrote.
        for device in devices.iter().filter(|device| {
            own.is_some_and(|own| own.me != **device && own.devices.contains(device))
        }) {
            self.record(device, space, FanoutStanding::Held, now_ms);
        }
        self.ledger
            .lock_recovering()
            .insert(space.to_string(), devices);
    }

    /// Whether `(device, space)` is worth asking now. Nothing recorded is
    /// due; `Held` never is; a device that could not be asked is due at its
    /// retry time, or at once when woken.
    ///
    /// A `Declined` is due never. A person said no on that device, and a
    /// loop that asked again every hour would be a loop that overrules
    /// them on the second hour; the standing is memory, so a restart asks
    /// once more and takes the answer again. A `Refused` is a condition
    /// rather than an answer, and does come round again.
    fn due(&self, device: &DeviceId, space: &str, now_ms: u64, woken: bool) -> bool {
        let recorded = self
            .standings
            .lock_recovering()
            .get(&(device.clone(), space.to_string()))
            .cloned();
        let Some(recorded) = recorded else {
            return true;
        };
        let lifetime = u64::try_from(REFUSAL_LIFETIME.as_millis()).unwrap_or(u64::MAX);
        let floor = u64::try_from(RETRY_FLOOR.as_millis()).unwrap_or(u64::MAX);
        match &recorded.standing {
            FanoutStanding::Held | FanoutStanding::Declined { .. } => false,
            FanoutStanding::CouldNotAsk { retry_at_ms, .. } => woken || now_ms >= *retry_at_ms,
            FanoutStanding::Deferred { .. } => {
                woken || now_ms >= recorded.at_ms.saturating_add(floor)
            }
            FanoutStanding::Refused { .. } => now_ms >= recorded.at_ms.saturating_add(lifetime),
        }
    }
}

/// The fan-out, both sides: the holder's reconcile loop, and the answerer
/// on the Own lane. Runs until `stop`. A device set that changed is the
/// one thing that skips a backoff: whatever moved in it is worth asking
/// about now rather than at the end of somebody's retry delay. `own` is
/// `None` until the set is restored, and then nothing is offered or
/// answered — the hub admits nobody on the lane either.
pub(crate) async fn serve(
    router: Arc<Router>,
    transport: Arc<dyn Transport>,
    mut own: watch::Receiver<Option<OwnDevices>>,
    facts: Arc<Facts>,
    mut stop: watch::Receiver<bool>,
) {
    // What this daemon knew about its siblings before it restarted. Taught
    // first, because the announcement below is a dial and a dial needs an
    // address.
    own_routes::teach(router.catalog().identity(), transport.as_ref());
    let answerer = tokio::spawn(answer_loop(
        router.clone(),
        transport.clone(),
        facts.clone(),
        stop.clone(),
    ));
    let mut doorbells = router.subscribe();
    let mut bells_open = true;
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut woken = false;
    // Say where this device is before asking anything of anybody: this
    // daemon may be the one that just came back on a new port.
    let mut announce_all = true;
    let mut announcements: BTreeMap<DeviceId, u64> = BTreeMap::new();
    let mut failures: BTreeMap<DeviceId, u32> = BTreeMap::new();
    // The set as this loop last saw it, and the devices it stopped naming.
    // A retirement is a fact about the profile; de-listing the device in
    // every Space is a separate signed act per Space, and the diff is what
    // asks for it — whether the retirement happened here or on the other
    // machine of this person's that heard it first.
    let mut known: Vec<DeviceId> = own
        .borrow()
        .as_ref()
        .map_or_else(Vec::new, |set| set.devices.clone());
    let mut gone: Vec<DeviceId> = Vec::new();
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            changed = own.changed() => {
                if changed.is_err() {
                    break;
                }
                // A set that just changed is worth asking about now — and
                // worth telling, because what moved in it may be a device
                // that has never heard where this one is.
                woken = true;
                announce_all = true;
                if let Some(set) = own.borrow().as_ref() {
                    for device in &known {
                        if !set.devices.contains(device)
                            && *device != set.me
                            && !gone.contains(device)
                        {
                            gone.push(device.clone());
                        }
                    }
                    known.clone_from(&set.devices);
                }
            }
            bell = doorbells.recv(), if bells_open => match bell {
                Ok(bell) if bell.doorbell.authority_advanced => {
                    refresh_orbit(&router, &facts, &bell.orbit).await;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => bells_open = false,
            },
            _ = interval.tick() => {
                let Some(set) = own.borrow().clone() else {
                    continue;
                };
                // Retired by another device of the profile. This daemon keeps
                // its seed and every store it holds — nothing here deletes
                // either — and stops speaking on the lane: it offers nothing,
                // announces nothing, and answers nothing.
                if !set.devices.contains(&set.me) {
                    continue;
                }
                // A device the set no longer names, de-listed in every Space
                // whose ledger still names it. One per tick, and one Space at
                // a time inside it, for the reason offers are: this places a
                // Station per Space it reads.
                if let Some(device) = gone.pop() {
                    tokio::select! {
                        () = async {
                            let held = holdings(&router, &facts).await;
                            let delisted = de_list(&router, &held, &device).await;
                            if !delisted.revoked_in.is_empty() {
                                tracing::info!(
                                    target: "lait::fanout",
                                    device = %device,
                                    revoked_in = ?delisted.revoked_in,
                                    unfenced = ?delisted.unfenced,
                                    "a device the profile no longer names was de-listed"
                                );
                            }
                            facts.forget_device(&device);
                        } => {}
                        _ = stop.changed() => break,
                    }
                    continue;
                }
                let now = crate::daemon::pair::now_ms();
                if std::mem::take(&mut announce_all) {
                    announcements = set
                        .devices
                        .iter()
                        .filter(|device| **device != set.me)
                        .map(|device| (device.clone(), now))
                        .collect();
                    failures.clear();
                }
                let greet = announcements
                    .iter()
                    .find(|(_, due)| **due <= now)
                    .map(|(device, _)| device.clone());
                let skip_backoff = std::mem::take(&mut woken);
                // A dial can sit for the answer deadline, and an
                // announcement is a dial. Selected against the stop so a
                // shutdown lands inside the bound the daemon joins this task
                // with, rather than after whatever the network was doing
                // when it was asked to end. One dial per tick either way,
                // and saying where this device is comes first, because every
                // offer after it depends on being reachable.
                tokio::select! {
                    () = async {
                        match &greet {
                            Some(device) => announce_one(
                                transport.as_ref(),
                                router.catalog().identity(),
                                &facts,
                                device,
                                &mut announcements,
                                &mut failures,
                                now,
                            ).await,
                            None => {
                                step(&router, transport.as_ref(), &set, &facts, skip_backoff).await;
                            }
                        }
                    } => {}
                    _ = stop.changed() => break,
                }
                // An announcement took this tick, so the offer it would have
                // made is still owed: do not spend the wake on it.
                if greet.is_some() {
                    woken = skip_backoff;
                }
            }
        }
    }
    if let Err(error) = tokio::time::timeout(Duration::from_secs(5), answerer).await {
        tracing::debug!(%error, "the Own lane answerer did not finish in time");
    }
}

/// Tell one own device where this one is.
///
/// Deliberately not discovery: the only address that travels is this
/// device's own, only to a device already in the profile's set, at start and
/// when that set changes. What it repairs is the restart — a daemon comes
/// back on a new ephemeral port, and with no relay to resolve a bare id the
/// route its siblings hold is an address nobody answers on. The side that is
/// still reachable takes the announcement, and both directions work again. A
/// device that does not answer is tried again on the ordinary backoff and no
/// more often than that.
async fn announce_one(
    transport: &dyn Transport,
    identity: &std::path::Path,
    facts: &Facts,
    device: &DeviceId,
    announcements: &mut BTreeMap<DeviceId, u64>,
    failures: &mut BTreeMap<DeviceId, u32>,
    now: u64,
) {
    let routes = transport
        .advertised_routes(ROUTES_DEADLINE)
        .await
        .unwrap_or_default();
    let hello = OwnFrame::Hello { routes };
    match tokio::time::timeout(ANSWER_DEADLINE, exchange(transport, device, &hello)).await {
        Ok(Ok(answer)) => {
            if let OwnAnswer::Learned { routes } = answer {
                own_routes::remember(identity, Some(transport), device, &routes);
            }
            // It answered, so this daemon can reach it: whatever it last
            // recorded about not reaching it was taken before now.
            facts.reachable_again(device);
            announcements.remove(device);
            failures.remove(device);
        }
        Ok(Err(_)) | Err(_) => {
            let tries = failures.entry(device.clone()).or_insert(0);
            let delay = u64::try_from(backoff(*tries).as_millis()).unwrap_or(u64::MAX);
            *tries = tries.saturating_add(1);
            announcements.insert(device.clone(), now.saturating_add(delay));
            tracing::debug!(
                target: "lait::fanout",
                device = %device,
                "could not say where this device is; trying again later"
            );
        }
    }
}

/// The Spaces this daemon serves under its own key: registered, present,
/// and signed with this identity's seed — never an agent's.
fn own_spaces(router: &Router) -> Vec<(String, PathBuf)> {
    let mut seen = std::collections::BTreeSet::new();
    router
        .catalog()
        .bindings()
        .into_iter()
        .filter(|binding| binding.identity == StationIdentity::Own)
        .filter(|binding| orbits::presence(&binding.entry) == orbits::Presence::Present)
        .filter(|binding| seen.insert(binding.entry.space.clone()))
        .map(|binding| (binding.entry.space, PathBuf::from(binding.entry.path)))
        .collect()
}

fn route_for(space: &str, path: &std::path::Path) -> Option<ControlRoute> {
    let space = SpaceId::parse(space)?;
    Some(ControlRoute::Orbit {
        address: OrbitAddress::for_store(path, space),
    })
}

/// Read the ledger's device list for one placed Orbit and take it as fact.
async fn read_ledger(
    router: &Router,
    facts: &Facts,
    route: &ControlRoute,
    space: &str,
) -> Result<Vec<DeviceId>> {
    match router
        .request_routed(route.clone(), &Request::DeviceList, None)
        .await?
    {
        Response::Text { text } => {
            let devices = parse_device_list(&text);
            let own = router.correspondence().own_devices().borrow().clone();
            facts.note_ledger(
                space,
                devices.clone(),
                own.as_ref(),
                crate::daemon::pair::now_ms(),
            );
            Ok(devices)
        }
        Response::Error { message, .. } => Err(anyhow!(message)),
        _ => Err(anyhow!("the device list came back in an unexpected shape")),
    }
}

/// `DeviceList` answers one device per line, the local one tagged.
fn parse_device_list(text: &str) -> Vec<DeviceId> {
    text.lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(DeviceId::parse)
        .collect()
}

/// A placed Orbit's authority advanced: re-read its ledger so a device that
/// just landed reads as `Held` without waiting for its turn.
async fn refresh_orbit(router: &Router, facts: &Facts, orbit: &crate::daemon::LocalOrbitId) {
    let Some((space, path)) = own_spaces(router)
        .into_iter()
        .find(|(_, path)| crate::daemon::LocalOrbitId::for_store(path) == *orbit)
    else {
        return;
    };
    let Some(route) = route_for(&space, &path) else {
        return;
    };
    if let Err(error) = read_ledger(router, facts, &route, &space).await {
        tracing::debug!(space, %error, "could not re-read the device list after the authority advanced");
    }
}

/// One Space this daemon holds, as a de-listing has to see it.
struct Holding {
    space: String,
    route: ControlRoute,
    /// The devices this Space's ledger binds to my actor here.
    devices: Vec<DeviceId>,
    /// Whether the actor answering here may rotate the Space key. A revoke
    /// signed by a non-admin de-lists and fences nothing, and that is a fact
    /// about one Space rather than about the retirement.
    admin: bool,
}

/// Every Space this daemon serves under its own key, with its ledger read
/// and its own standing in it.
///
/// Reading is what places the Station, so this is the expensive half of any
/// de-listing and is done once for the whole act rather than per device.
async fn holdings(router: &Router, facts: &Facts) -> Vec<Holding> {
    let mut held = Vec::new();
    for (space, path) in own_spaces(router) {
        if let Some(holding) = holding_of(router, facts, &space, &path).await {
            held.push(holding);
        }
    }
    held
}

/// One Space, read: who its ledger names under my actor, and whether that
/// actor may rotate the key here.
async fn holding_of(
    router: &Router,
    facts: &Facts,
    space: &str,
    path: &std::path::Path,
) -> Option<Holding> {
    let route = route_for(space, path)?;
    let devices = match read_ledger(router, facts, &route, space).await {
        Ok(devices) => devices,
        Err(error) => {
            tracing::debug!(space, %error, "could not read this Space's device list");
            return None;
        }
    };
    let admin = matches!(
        router.request_routed(route.clone(), &Request::Status, None).await,
        Ok(Response::Status(info)) if info.membership == "admin"
    );
    Some(Holding {
        space: space.to_string(),
        route,
        devices,
        admin,
    })
}

/// The Spaces `device` is the last device of this person's actor in.
///
/// Pure, because it is a refusal and a refusal has to be arguable without a
/// network: a retirement that emptied an actor would leave a Space nobody
/// could ever rotate the key of or admit anyone to again, while the retired
/// machine went on reading everything already sealed to it. Losing that is
/// not worth automating, so the whole retirement is refused and says which
/// Space it was.
fn orphaned_by(ledgers: &[(String, Vec<DeviceId>)], device: &DeviceId) -> Vec<String> {
    ledgers
        .iter()
        .filter(|(_, devices)| {
            devices.contains(device) && devices.iter().all(|held| held == device)
        })
        .map(|(space, _)| space.clone())
        .collect()
}

/// What a de-listing cost, per Space.
struct Delisted {
    /// The Spaces whose ledger stopped naming the device — one signed op
    /// each, authored here.
    revoked_in: Vec<String>,
    /// The subset of those where nobody could rotate the Space key
    /// afterwards, so the device can still read what it already held.
    /// Reported apart from the first list because "de-listed" and
    /// "de-listed and fenced" are different facts and only one ends access.
    unfenced: Vec<String>,
}

/// Remove `device` from my actor in every Space this daemon holds that names
/// it: one signed `RevokeDevice` per Space.
///
/// Never derived from the kinship act that retired it. Kinship says who is a
/// device of this person and authorizes nothing; a Space stops naming a
/// device only because a device of its actor signed that it should, which is
/// the same op a person reaches by hand.
///
/// A Space that answers "not bound" has already converged — another device of
/// the profile reacted to the same retirement — and records nothing: a race
/// that both sides handled is not a refusal.
async fn de_list(router: &Router, holdings: &[Holding], device: &DeviceId) -> Delisted {
    let mut delisted = Delisted {
        revoked_in: Vec::new(),
        unfenced: Vec::new(),
    };
    for holding in holdings
        .iter()
        .filter(|holding| holding.devices.contains(device))
    {
        let asked = router
            .request_routed(
                holding.route.clone(),
                &Request::DeviceRevoke {
                    device: device.as_str().to_owned(),
                },
                None,
            )
            .await;
        match asked {
            Ok(Response::Ok { .. }) => {
                delisted.revoked_in.push(holding.space.clone());
                if !holding.admin {
                    delisted.unfenced.push(holding.space.clone());
                }
                tracing::info!(
                    target: "lait::fanout",
                    space = %holding.space,
                    device = %device,
                    fenced = holding.admin,
                    "de-listed"
                );
            }
            Ok(Response::Error { message, .. }) if message.contains("not bound") => {}
            Ok(other) => tracing::warn!(
                space = %holding.space,
                "the Space answered the revoke in an unexpected shape: {other:?}"
            ),
            Err(error) => tracing::warn!(
                space = %holding.space,
                %error,
                "could not de-list the device in this Space"
            ),
        }
    }
    delisted
}

/// Retire one of this profile's devices, and de-list it everywhere.
///
/// The order is the whole design. The Spaces are read and the refusal is
/// decided **before** anything is signed, because a retirement that got
/// halfway would leave a device the profile no longer names still named by
/// every ledger. Then the kinship entry — which drops the device from the
/// watch, and with it from the hub's admission, the fan-out and the tunnel's
/// routes — and only then one signed actor op per Space.
///
/// Nothing here deletes a seed or a store byte, on either machine. A retired
/// device keeps everything it holds and simply stops being spoken to.
pub(crate) async fn retire(router: &Router, facts: &Facts, device: &str) -> Response {
    let Some(device) = DeviceId::parse(device) else {
        return Response::invalid("that is not a device id");
    };
    let correspondence = router.correspondence();
    let Some(own) = correspondence.own_devices().borrow().clone() else {
        return Response::err("the device set is not held on this daemon");
    };
    // The plane refuses both of these too. Refusing here as well is what
    // keeps a mistyped id from placing a Station and reading a ledger first.
    if device == own.me {
        return Response::err("retire this device from another one");
    }
    if !own.devices.contains(&device) {
        return Response::err("that device is not one of this profile's");
    }
    let holdings = holdings(router, facts).await;
    let ledgers: Vec<(String, Vec<DeviceId>)> = holdings
        .iter()
        .map(|holding| (holding.space.clone(), holding.devices.clone()))
        .collect();
    let orphaned = orphaned_by(&ledgers, &device);
    if !orphaned.is_empty() {
        return Response::err(format!(
            "retiring that device would leave {} with no device of yours — nobody could \
             rotate the Space key or admit anyone there again",
            orphaned.join(", ")
        ));
    }
    if let Err(error) =
        correspondence.retire_device(&device, crate::daemon::correspondence::now_secs())
    {
        return Response::err(error);
    }
    let delisted = de_list(router, &holdings, &device).await;
    facts.forget_device(&device);
    Response::Host(crate::control::HostReply::DeviceRetired {
        device: device.as_str().to_owned(),
        revoked_in: delisted.revoked_in,
        unfenced: delisted.unfenced,
    })
}

/// One step: the first (device, Space) pair worth asking about, asked.
async fn step(
    router: &Router,
    transport: &dyn Transport,
    own: &OwnDevices,
    facts: &Facts,
    skip_backoff: bool,
) {
    let now = crate::daemon::pair::now_ms();
    let peers: Vec<&DeviceId> = own.devices.iter().filter(|d| **d != own.me).collect();
    if peers.is_empty() {
        return;
    }
    let spaces = own_spaces(router);
    let Some((device, space, path)) = spaces.iter().find_map(|(space, path)| {
        peers
            .iter()
            .find(|device| facts.due(device, space, now, skip_backoff))
            .map(|device| ((*device).clone(), space.clone(), path.clone()))
    }) else {
        return;
    };
    offer_one(router, transport, facts, &device, &space, &path, now).await;
}

async fn offer_one(
    router: &Router,
    transport: &dyn Transport,
    facts: &Facts,
    device: &DeviceId,
    space: &str,
    path: &std::path::Path,
    now: u64,
) {
    let Some(route) = route_for(space, path) else {
        facts.record(
            device,
            space,
            FanoutStanding::Deferred {
                why: "the registry names a malformed Space id".into(),
            },
            now,
        );
        return;
    };
    // The ledger first: it places the Station, and it may already answer.
    match read_ledger(router, facts, &route, space).await {
        Ok(devices) if devices.contains(device) => return,
        Ok(_) => {}
        Err(error) => {
            facts.record(
                device,
                space,
                FanoutStanding::Deferred {
                    why: format!("{error:#}"),
                },
                now,
            );
            return;
        }
    }
    let (link, actor) = match router
        .request_routed(route.clone(), &Request::Coordinates, None)
        .await
    {
        Ok(Response::Coordinates {
            link,
            actor,
            space: minted_for,
        }) if minted_for == space => (link, actor),
        Ok(Response::Coordinates { .. }) => {
            facts.record(
                device,
                space,
                FanoutStanding::Deferred {
                    why: "the ticket names another Space".into(),
                },
                now,
            );
            return;
        }
        Ok(Response::Error { message, .. }) => {
            facts.record(
                device,
                space,
                FanoutStanding::Deferred { why: message },
                now,
            );
            return;
        }
        Ok(_) => {
            facts.record(
                device,
                space,
                FanoutStanding::Deferred {
                    why: "the ticket came back in an unexpected shape".into(),
                },
                now,
            );
            return;
        }
        Err(error) => {
            facts.record(
                device,
                space,
                FanoutStanding::Deferred {
                    why: format!("{error:#}"),
                },
                now,
            );
            return;
        }
    };
    // Under an isolated network a ticket without direct routes is one the
    // other side can never dial; offering it would only earn a Contact that
    // fails for thirty seconds. Wait for the Station's routes instead.
    if transport.is_isolated() {
        let routed = runtime::coordinates::SignedCoordinates::parse_link(&link)
            .ok()
            .and_then(|ticket| ticket.verify().ok())
            .is_some_and(|verified| !verified.approach_routes.is_empty());
        if !routed {
            facts.record(
                device,
                space,
                FanoutStanding::Deferred {
                    why: "no routes".into(),
                },
                now,
            );
            return;
        }
    }
    let routes = transport
        .advertised_routes(ROUTES_DEADLINE)
        .await
        .unwrap_or_default();
    let offer = OwnFrame::Offer {
        space: space.to_string(),
        actor,
        coordinates: link,
        routes,
    };
    let answer =
        match tokio::time::timeout(ANSWER_DEADLINE, exchange(transport, device, &offer)).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(error)) => {
                facts.could_not_ask(device, space, format!("{error:#}"), now);
                return;
            }
            Err(_) => {
                facts.could_not_ask(
                    device,
                    space,
                    format!("no answer within {} s", ANSWER_DEADLINE.as_secs()),
                    now,
                );
                return;
            }
        };
    // Whatever it said, it answered: the dial-failure measurement that may
    // be sitting on this device is now stale, and stale is not current.
    facts.answered(device);
    let standing = match answer {
        OwnAnswer::Consent { binding } => {
            match router
                .request_routed(
                    route.clone(),
                    &Request::DeviceAdd { consent: binding },
                    None,
                )
                .await
            {
                Ok(Response::Ok { .. }) => match read_ledger(router, facts, &route, space).await {
                    Ok(devices) if devices.contains(device) => return,
                    Ok(_) => FanoutStanding::Refused {
                        why: "added, but the ledger does not name the device".into(),
                    },
                    Err(error) => FanoutStanding::Deferred {
                        why: format!("{error:#}"),
                    },
                },
                Ok(Response::Error { message, .. }) => FanoutStanding::Refused { why: message },
                Ok(_) => FanoutStanding::Refused {
                    why: "the Space answered the add in an unexpected shape".into(),
                },
                Err(error) => FanoutStanding::Deferred {
                    why: format!("{error:#}"),
                },
            }
        }
        OwnAnswer::Held => FanoutStanding::Held,
        OwnAnswer::Declined { why } => FanoutStanding::Declined { why },
        OwnAnswer::Refused { why } => FanoutStanding::Refused { why },
        OwnAnswer::Learned { .. } => FanoutStanding::Deferred {
            why: "the device answered an offer with its routes".into(),
        },
    };
    tracing::info!(
        target: "lait::fanout",
        space,
        device = %device,
        standing = ?standing,
        "offered"
    );
    facts.record(device, space, standing, now);
}

/// Dial, say one frame, hear one frame.
async fn exchange(
    transport: &dyn Transport,
    device: &DeviceId,
    frame: &OwnFrame,
) -> Result<OwnAnswer> {
    let bytes = postcard::to_stdvec(frame).context("encode the offer")?;
    if bytes.len() > MAX_OWN_FRAME {
        bail!("the offer is larger than the lane allows");
    }
    let mut stream = transport
        .connect(device.clone(), OWN_ALPN)
        .await
        .context("dial the device")?;
    stream.send(&bytes).await.context("send the offer")?;
    let reply = stream
        .recv_bounded(MAX_OWN_FRAME)
        .await
        .context("hear the answer")?
        .ok_or_else(|| anyhow!("the device closed without answering"))?;
    postcard::from_bytes(&reply).context("decode the answer")
}

/// The Spaces this device is entering right now, and how many it may enter
/// at once.
///
/// The set is what keeps one Space out of two entries at the same time.
/// `bootstrap::enter` is idempotent only once the store exists, so two
/// tasks aimed at one directory would both run `enter_space` on it — and a
/// duplicate offer is ordinary, not exotic: the holder gives up at its
/// answer deadline and asks again, and a third Space queues behind the
/// permits long enough for the offer to be repeated.
struct Entering {
    permits: Semaphore,
    in_flight: Mutex<std::collections::BTreeSet<String>>,
}

impl Entering {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            permits: Semaphore::new(ENTER_CONCURRENCY),
            in_flight: Mutex::new(std::collections::BTreeSet::new()),
        })
    }

    /// Claim `space` for this task, or `None` when another task holds it.
    fn claim(self: &Arc<Self>, space: &str) -> Option<Claim> {
        self.in_flight
            .lock_recovering()
            .insert(space.to_string())
            .then(|| Claim {
                entering: self.clone(),
                space: space.to_string(),
            })
    }
}

/// One Space's claim on the entering set, released when the task ends —
/// including when it ends by panicking, which is the whole reason it is a
/// guard rather than a pair of calls.
struct Claim {
    entering: Arc<Entering>,
    space: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.entering
            .in_flight
            .lock_recovering()
            .remove(&self.space);
    }
}

/// Whether this daemon is still a device of its own profile.
///
/// A device retired by another device of the person's is out of the set it
/// publishes itself, and the fan-out is the first thing that has to notice:
/// it goes on holding every store and its own seed, and stops offering,
/// announcing and answering. `None` — the set is not restored — is out too,
/// for the reason the hub's admission is: unmeasured is absent.
fn still_own(router: &Router) -> bool {
    router
        .correspondence()
        .own_devices()
        .borrow()
        .as_ref()
        .is_some_and(|own| own.devices.contains(&own.me))
}

/// The other side of the lane. Everything that arrives here was admitted
/// by the hub against the device set; what is checked here is the offer
/// itself — that the ticket was signed by the device offering it, for the
/// Space it says.
async fn answer_loop(
    router: Arc<Router>,
    transport: Arc<dyn Transport>,
    facts: Arc<Facts>,
    mut stop: watch::Receiver<bool>,
) {
    let entering = Entering::new();
    let mut said_nothing = false;
    loop {
        let incoming = tokio::select! {
            incoming = transport.accept() => incoming,
            _ = stop.changed() => break,
        };
        match incoming {
            Some(incoming) => {
                said_nothing = false;
                tokio::spawn(answer_one(
                    router.clone(),
                    transport.clone(),
                    facts.clone(),
                    entering.clone(),
                    incoming,
                ));
            }
            // `None` is also what the lane answers while another view holds
            // it, not only at shutdown; leaving here would leave the daemon
            // deaf to its own devices for the rest of its life.
            None => {
                if *stop.borrow() {
                    break;
                }
                if !said_nothing {
                    tracing::warn!("the Own lane handed over nothing; listening again shortly");
                    said_nothing = true;
                }
                tokio::select! {
                    () = tokio::time::sleep(TICK) => {}
                    _ = stop.changed() => break,
                }
            }
        }
    }
}

async fn answer_one(
    router: Arc<Router>,
    transport: Arc<dyn Transport>,
    facts: Arc<Facts>,
    entering: Arc<Entering>,
    mut incoming: Incoming,
) {
    let from = incoming.from.clone();
    // A device this profile no longer names does not answer for it. The hub
    // still admits the caller — the set it admits on is this device's own
    // reading, and being retired does not make a sibling a stranger — but a
    // machine that is out of the set has nothing to say on this lane and
    // must not consent itself into anything.
    if !still_own(&router) {
        return;
    }
    let frame: OwnFrame =
        match tokio::time::timeout(ANSWER_DEADLINE, incoming.stream.recv_bounded(MAX_OWN_FRAME))
            .await
        {
            Ok(Ok(Some(bytes))) => match postcard::from_bytes(&bytes) {
                Ok(frame) => frame,
                Err(_) => return,
            },
            // Oversized, ended, or late: dropped unread. Nothing is owed to a
            // frame that did not arrive whole.
            _ => return,
        };
    // Before anything else, and whatever the frame says: this is where the
    // device that sent it can be reached, and the fact that it sent one at
    // all is what makes any older record of not reaching it stale. Kept, so
    // that a restart of this daemon does not forget the only address it has
    // for a device it can no longer ask.
    crate::daemon::own_routes::remember(
        router.catalog().identity(),
        Some(transport.as_ref()),
        &from,
        frame.routes(),
    );
    facts.reachable_again(&from);
    let answer = match frame {
        OwnFrame::Offer {
            space,
            actor,
            coordinates,
            ..
        } => answer_offer(router, &from, &space, &actor, coordinates, entering).await,
        OwnFrame::Hello { .. } => OwnAnswer::Learned {
            routes: transport
                .advertised_routes(ROUTES_DEADLINE)
                .await
                .unwrap_or_default(),
        },
    };
    let Ok(bytes) = postcard::to_stdvec(&answer) else {
        return;
    };
    if tokio::time::timeout(ANSWER_DEADLINE, incoming.stream.send(&bytes))
        .await
        .is_err()
    {
        return;
    }
    let _ = incoming.stream.finish().await;
    // Accept-side contract: the dialer drains, then closes; dropping first
    // would truncate the answer it has not yet read.
    let _ = tokio::time::timeout(ANSWER_DEADLINE, incoming.stream.wait_closed()).await;
}

/// The offer, checked: the ticket verifies, was signed by the device that
/// offered it, and names the Space the offer says. An own device relaying
/// somebody else's ticket would otherwise consent this device into whatever
/// actor that offer named.
fn verified_offer(
    from: &DeviceId,
    space: &str,
    coordinates: &str,
) -> Result<runtime::coordinates::VerifiedCoordinates, String> {
    let verified = runtime::coordinates::SignedCoordinates::parse_link(coordinates)
        .and_then(|ticket| ticket.verify())
        .map_err(|error| format!("the ticket does not verify: {error}"))?;
    if verified.approach_station != *from {
        return Err("ticket not signed by the offering device".into());
    }
    if verified.space.as_str() != space {
        return Err("the ticket names another Space".into());
    }
    Ok(verified)
}

/// The actor this device already answers as in a Space it holds — `None`
/// until it is a member of one, which a device that has entered but not yet
/// been admitted is not.
async fn member_actor(router: &Router, home: &std::path::Path, space: &SpaceId) -> Option<String> {
    let route = ControlRoute::Orbit {
        address: OrbitAddress::for_store(home, space.clone()),
    };
    match router.request_routed(route, &Request::Whoami, None).await {
        Ok(Response::Whoami(who)) => who.member.then_some(who.actor).flatten(),
        _ => None,
    }
}

async fn answer_offer(
    router: Arc<Router>,
    from: &DeviceId,
    space: &str,
    actor: &str,
    coordinates: String,
    entering: Arc<Entering>,
) -> OwnAnswer {
    let verified = match verified_offer(from, space, &coordinates) {
        Ok(verified) => verified,
        Err(why) => return OwnAnswer::Refused { why },
    };
    if mechanics::ids::ActorId::parse(actor).is_none() {
        return OwnAnswer::Refused {
            why: "the offer names a malformed actor".into(),
        };
    }
    // Already a member here: nothing to consent to — but only under the
    // actor the offer names. A device that entered this Space as a person
    // of its own before it was ever paired is a member under a different
    // actor, and answering `Held` would put a hold on the holder's books
    // that its ledger will never name and nothing will ever ask about
    // again. Only a store this machine registered is asked, and asking
    // places it.
    let registered = bootstrap::registered_home(&router, space);
    if let Some(home) = &registered {
        match member_actor(&router, home, &verified.space).await {
            Some(mine) if mine == actor => return OwnAnswer::Held,
            Some(_) => {
                return OwnAnswer::Refused {
                    why: "held under another actor".into(),
                }
            }
            None => {}
        }
    }
    let identity = router.catalog().identity().to_path_buf();
    let token = format!("{actor} {space}");
    let binding =
        match tokio::task::spawn_blocking(move || bootstrap::device_consent(&identity, &token))
            .await
        {
            Ok(Ok(binding)) => binding,
            Ok(Err(error)) => {
                return OwnAnswer::Refused {
                    why: format!("{error:#}"),
                }
            }
            Err(error) => {
                return OwnAnswer::Refused {
                    why: format!("consent task failed: {error}"),
                }
            }
        };
    // Enter on our own time, bounded: the holder adds this device the moment
    // it has the consent, and admission converges through Contact whichever
    // of the two lands first.
    let home = registered.unwrap_or_else(|| bootstrap::allocated_home(space));
    let space = space.to_string();

    // Consent is owed either way — the holder asked and this device agrees —
    // but the entry is not started twice. A repeat offer while the first
    // entry is still in flight is answered and dropped here.
    let Some(claim) = entering.claim(&space) else {
        tracing::debug!(
            target: "lait::fanout",
            space,
            "already entering this Space; consenting again without a second entry"
        );
        return OwnAnswer::Consent { binding };
    };
    tokio::spawn(async move {
        let _claim = claim;
        let Ok(_permit) = entering.permits.acquire().await else {
            return;
        };
        let home = home.to_string_lossy().into_owned();
        match bootstrap::enter_and_await(&router, &home, &coordinates, None).await {
            Ok((entered, admission)) => tracing::info!(
                target: "lait::fanout",
                space,
                home = %entered.home.display(),
                admitted = admission.admitted,
                contacted = admission.contacted,
                last_error = admission.last_error.as_deref().unwrap_or(""),
                "entered"
            ),
            Err(refusal) => tracing::warn!(
                target: "lait::fanout",
                space,
                refusal = ?refusal,
                "could not enter the offered Space"
            ),
        }
    });
    OwnAnswer::Consent { binding }
}

#[cfg(test)]
// The env guard is held for a whole test by design; tightening it is the
// bug the lock exists to prevent.
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use comms::mem::MemNet;
    use comms::policy::Network;
    use comms::TransportFactory;
    use mechanics::actor::device_from_seed;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::MutexGuard;

    // `LAIT_CONFIG_ROOT` is process-global: the registry every side reads
    // lives under it, so these tests take the env for their whole run.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct ScopedRoot {
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl ScopedRoot {
        fn new(tag: &str) -> Self {
            let guard = ENV_LOCK.lock_recovering();
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("lait-fanout-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("root");
            // Registered store paths are resolved through the filesystem;
            // the roots the catalogs filter on must be spelled the same way.
            let dir = dir.canonicalize().expect("canonical root");
            std::env::set_var("LAIT_CONFIG_ROOT", &dir);
            Self { dir, _guard: guard }
        }
    }

    impl Drop for ScopedRoot {
        fn drop(&mut self) {
            std::env::remove_var("LAIT_CONFIG_ROOT");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    struct MemFactory(MemNet);

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _protocols: comms::Protocols<'_>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(self.0.peer(device_from_seed(identity_seed))))
        }
    }

    /// A transport whose route to one device is stale: the dial fails until
    /// something teaches it an address for that device again. What a peer
    /// that restarted on a new ephemeral port looks like from the other
    /// side, under a policy with no relay to resolve a bare id — which
    /// `MemNet` cannot show on its own, because it resolves by id and needs
    /// no addresses at all.
    struct StaleRoute {
        inner: Arc<dyn Transport>,
        stale: Mutex<std::collections::BTreeSet<DeviceId>>,
    }

    impl StaleRoute {
        fn new(inner: Arc<dyn Transport>, gone: DeviceId) -> Arc<Self> {
            Arc::new(Self {
                inner,
                stale: Mutex::new(std::iter::once(gone).collect()),
            })
        }

        fn is_stale(&self, device: &DeviceId) -> bool {
            self.stale.lock_recovering().contains(device)
        }
    }

    #[async_trait]
    impl Transport for StaleRoute {
        fn my_id(&self) -> comms::PeerId {
            self.inner.my_id()
        }

        fn learn(&self, peer: comms::PeerId, addrs: &[SocketAddr]) {
            if !addrs.is_empty() {
                self.stale.lock_recovering().remove(&peer);
            }
            self.inner.learn(peer, addrs);
        }

        async fn connect(
            &self,
            peer: comms::PeerId,
            alpn: comms::Alpn,
        ) -> Result<Box<dyn comms::Stream>> {
            if self.is_stale(&peer) {
                bail!("no route to {peer}");
            }
            self.inner.connect(peer, alpn).await
        }

        async fn accept(&self) -> Option<Incoming> {
            self.inner.accept().await
        }

        fn advertised_addrs(&self) -> Vec<SocketAddr> {
            self.inner.advertised_addrs()
        }

        async fn subscribe(
            &self,
            topic: comms::Topic,
            bootstrap: &[comms::PeerId],
        ) -> Result<(Box<dyn comms::GossipSender>, Box<dyn comms::GossipReceiver>)> {
            self.inner.subscribe(topic, bootstrap).await
        }

        async fn shutdown(&self) {
            self.inner.shutdown().await;
        }
    }

    /// A transport with an address to advertise. `MemNet` has none, and a
    /// Hello carrying no routes would prove nothing about the repair.
    struct Announcing {
        inner: Arc<dyn Transport>,
        addrs: Vec<SocketAddr>,
    }

    #[async_trait]
    impl Transport for Announcing {
        fn my_id(&self) -> comms::PeerId {
            self.inner.my_id()
        }

        fn learn(&self, peer: comms::PeerId, addrs: &[SocketAddr]) {
            self.inner.learn(peer, addrs);
        }

        async fn connect(
            &self,
            peer: comms::PeerId,
            alpn: comms::Alpn,
        ) -> Result<Box<dyn comms::Stream>> {
            self.inner.connect(peer, alpn).await
        }

        async fn accept(&self) -> Option<Incoming> {
            self.inner.accept().await
        }

        fn advertised_addrs(&self) -> Vec<SocketAddr> {
            self.addrs.clone()
        }

        async fn subscribe(
            &self,
            topic: comms::Topic,
            bootstrap: &[comms::PeerId],
        ) -> Result<(Box<dyn comms::GossipSender>, Box<dyn comms::GossipReceiver>)> {
            self.inner.subscribe(topic, bootstrap).await
        }

        async fn shutdown(&self) {
            self.inner.shutdown().await;
        }
    }

    /// One daemon's worth of fan-out: an identity home, a router over the
    /// shared network whose catalog sees only the stores under `roots`, the
    /// identity transport, and the loop once started.
    struct Side {
        home: PathBuf,
        seed: [u8; 32],
        router: Arc<Router>,
        transport: Arc<dyn Transport>,
        facts: Arc<Facts>,
        stop: watch::Sender<bool>,
        task: Option<tokio::task::JoinHandle<()>>,
    }

    impl Side {
        async fn stand(tag: &str, net: &MemNet, root: &ScopedRoot, roots: Vec<PathBuf>) -> Self {
            let home = root.dir.join(tag);
            std::fs::create_dir_all(&home).expect("home");
            let seed = crate::config::load_or_create_identity(&home).expect("identity");
            crate::config::identity_profile(&home).expect("profile");
            let mut roots = roots;
            roots.push(home.clone());
            let catalog = crate::orbits::Catalog::with_registry_view(
                home.clone(),
                home.join("agents"),
                false,
                Arc::new(move || {
                    orbits::list()
                        .into_iter()
                        .filter(|entry| roots.iter().any(|r| Path::new(&entry.path).starts_with(r)))
                        .collect()
                }),
            );
            let router = Arc::new(Router::with_factory(
                catalog,
                Arc::new(MemFactory(net.clone())),
                crate::world::packages(),
            ));
            let network = crate::config::Settings::load(Some(&home))
                .network()
                .expect("network policy");
            let transport = router
                .hub()
                .identity_transport(&seed, &network)
                .await
                .expect("identity transport");
            let facts = Facts::new();
            router.correspondence().hook_fanout(facts.clone());
            Self {
                home,
                seed,
                router,
                transport,
                facts,
                stop: watch::Sender::new(false),
                task: None,
            }
        }

        fn me(&self) -> DeviceId {
            device_from_seed(&self.seed)
        }

        /// Take `other` into this side's profile the way the ceremony does:
        /// a mutual link, adopted by the plane, which republishes the set.
        /// The watch alone is enough for the fan-out, but not for anything
        /// that writes to the kinship log — a retirement has to be signed
        /// against a profile that really names the device.
        fn adopt(&self, other: &Side) {
            let (me, them) = (self.me(), other.me());
            let (nonce, epoch) = ([57u8; 16], 2);
            let link = mechanics::kinship::DeviceLink::assemble(
                (
                    me.clone(),
                    mechanics::kinship::DeviceLink::half(&self.seed, &them, nonce, epoch),
                ),
                (
                    them,
                    mechanics::kinship::DeviceLink::half(&other.seed, &me, nonce, epoch),
                ),
                nonce,
                epoch,
            )
            .expect("assemble");
            self.router
                .correspondence()
                .adopt_device(link, crate::daemon::correspondence::now_secs())
                .expect("adopt");
        }

        /// Publish `devices` as this side's set, as pairing would have.
        fn own(&self, devices: &[DeviceId]) {
            let profile = self
                .router
                .correspondence()
                .own_devices()
                .borrow()
                .as_ref()
                .expect("restored")
                .profile
                .clone();
            let mut devices = devices.to_vec();
            devices.sort();
            self.router
                .correspondence()
                .set_own_for_test(Some(OwnDevices {
                    profile,
                    me: self.me(),
                    devices,
                }));
        }

        fn start(&mut self) {
            self.task = Some(tokio::spawn(serve(
                self.router.clone(),
                self.transport.clone(),
                self.router.correspondence().own_devices(),
                self.facts.clone(),
                self.stop.subscribe(),
            )));
        }

        async fn stop(mut self) {
            let _ = self.stop.send(true);
            if let Some(task) = self.task.take() {
                let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
            }
            let _ = self.router.shutdown().await;
        }

        fn found(&self, name: &str) -> (String, PathBuf) {
            let store = self.home.join(format!("store-{name}"));
            let founded = bootstrap::found(&self.router.packages(), &store, &self.home, name, None)
                .expect("found");
            (founded.space, founded.home)
        }

        async fn ask(&self, space: &str, store: &Path, request: Request) -> Response {
            self.router
                .request_routed(route_for(space, store).expect("route"), &request, None)
                .await
                .expect("routed")
        }

        async fn reach(&self) -> crate::control::ReachView {
            match self
                .router
                .correspondence()
                .handle(Request::ReachView)
                .await
            {
                Response::Reach(view) => *view,
                other => panic!("not a reach view: {other:?}"),
            }
        }
    }

    /// Link every side into one profile.
    fn link(sides: &[&Side]) {
        let devices: Vec<DeviceId> = sides.iter().map(|side| side.me()).collect();
        for side in sides {
            side.own(&devices);
        }
    }

    async fn poll_until<T, F, Fut>(deadline: Duration, mut check: F) -> Option<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        let started = tokio::time::Instant::now();
        while started.elapsed() < deadline {
            if let Some(value) = check().await {
                return Some(value);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        None
    }

    fn standing_in(
        view: &crate::control::ReachView,
        space: &str,
        device: &DeviceId,
    ) -> Option<FanoutStanding> {
        view.spaces
            .iter()
            .find(|row| row.space == space)?
            .standings
            .iter()
            .find(|row| row.device == device.as_str())
            .map(|row| row.standing.clone())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_space_fans_out_to_an_own_device_through_consent_and_device_add() {
        // A holds a Space; D is linked into A's profile by the watch alone.
        // Without a hand-carried token, D ends up a member — under A's
        // actor, not a second one — and A's ledger names both devices.
        let root = ScopedRoot::new("consent");
        let net = MemNet::new();
        let a = Side::stand("a", &net, &root, Vec::new()).await;
        let d = Side::stand("d", &net, &root, vec![crate::config::spaces_root()]).await;
        link(&[&a, &d]);
        let (space, a_store) = a.found("Fanned");
        let mut a = a;
        let mut d = d;
        a.start();
        d.start();

        let d_store = poll_until(Duration::from_secs(90), || async {
            let store = bootstrap::registered_home(&d.router, &space)?;
            match d.ask(&space, &store, Request::Status).await {
                // A device of the founder's actor reads `admin`, because
                // that is the actor's standing: the fact under test is that
                // it is a member at all, under one actor.
                Response::Status(info)
                    if matches!(info.membership.as_str(), "member" | "admin") =>
                {
                    Some(store)
                }
                _ => None,
            }
        })
        .await
        .expect("D became a member of the Space A holds");
        assert!(
            d_store.starts_with(crate::config::spaces_root()),
            "the entered store was allocated under the spaces root, not named by anyone"
        );

        let listed = poll_until(Duration::from_secs(30), || async {
            match a.ask(&space, &a_store, Request::DeviceList).await {
                Response::Text { text } => {
                    let devices = parse_device_list(&text);
                    (devices.len() == 2).then_some(devices)
                }
                _ => None,
            }
        })
        .await
        .expect("A's ledger names two devices");
        assert!(listed.contains(&a.me()) && listed.contains(&d.me()));
        match a.ask(&space, &a_store, Request::Members).await {
            Response::Members { members } => assert_eq!(
                members.len(),
                1,
                "one person, one actor — a second actor would be an invite, not a device"
            ),
            other => panic!("not a member list: {other:?}"),
        }

        let view = poll_until(Duration::from_secs(30), || async {
            let view = a.reach().await;
            (standing_in(&view, &space, &d.me()) == Some(FanoutStanding::Held)).then_some(view)
        })
        .await
        .expect("A's view records the Space as held on D");
        let row = view
            .devices
            .iter()
            .find(|row| row.device == d.me().as_str())
            .expect("D is in A's device rows");
        assert_eq!(
            row.held,
            vec![space.clone()],
            "held comes from the ledger A read"
        );
        let fanned = view.spaces.iter().find(|row| row.space == space).unwrap();
        assert!(fanned.on.contains(&d.me().as_str().to_owned()));

        a.stop().await;
        d.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_device_that_could_not_be_asked_is_neither_declined_nor_absent() {
        // D is cut off. A records that it could not ask — with a retry time,
        // never as a refusal — and the view keeps the row. Healed and woken,
        // the same row becomes held without waiting out the backoff.
        let root = ScopedRoot::new("partition");
        let net = MemNet::new();
        let a = Side::stand("a", &net, &root, Vec::new()).await;
        let d = Side::stand("d", &net, &root, vec![crate::config::spaces_root()]).await;
        link(&[&a, &d]);
        net.partition(&a.me(), &d.me());
        let (space, _) = a.found("Cut");
        let mut a = a;
        let mut d = d;
        a.start();
        d.start();

        let standing = poll_until(Duration::from_secs(45), || async {
            match a.facts.standing(&d.me(), &space) {
                Some(standing @ FanoutStanding::CouldNotAsk { .. }) => Some(standing),
                Some(other) => panic!("an unreachable device was recorded as {other:?}"),
                None => None,
            }
        })
        .await
        .expect("A recorded that D could not be asked");
        let FanoutStanding::CouldNotAsk { retry_at_ms, .. } = standing else {
            unreachable!()
        };
        assert!(
            retry_at_ms > crate::daemon::pair::now_ms(),
            "a retry is scheduled"
        );
        let view = a.reach().await;
        assert!(matches!(
            standing_in(&view, &space, &d.me()),
            Some(FanoutStanding::CouldNotAsk { .. })
        ));
        assert!(matches!(
            view.devices
                .iter()
                .find(|row| row.device == d.me().as_str())
                .map(|row| &row.liveness),
            Some(Liveness::CouldNotAsk { .. })
        ));
        assert!(
            !view
                .spaces
                .iter()
                .find(|row| row.space == space)
                .unwrap()
                .on
                .contains(&d.me().as_str().to_owned()),
            "the ledger never claimed D"
        );

        net.heal();
        // What a set change does in production, and the only thing that
        // skips a backoff: republished here with nothing altered.
        a.own(&[a.me(), d.me()]);
        // Well inside the thirty-second floor the retry was scheduled at.
        poll_until(Duration::from_secs(20), || async {
            (a.facts.standing(&d.me(), &space) == Some(FanoutStanding::Held)).then_some(())
        })
        .await
        .expect("healed and woken, the Space is held on D before the backoff would have run");
        // The measurement that said "could not be asked" was taken before
        // the device answered. It is not the current one, and a view that
        // kept showing it would be reporting a stale reading as fresh.
        assert_eq!(
            a.facts.liveness_of(&d.me()),
            Liveness::NotProbed,
            "a device that answered still reads as one that could not be asked"
        );
        assert!(matches!(
            a.reach()
                .await
                .devices
                .iter()
                .find(|row| row.device == d.me().as_str())
                .map(|row| &row.liveness),
            Some(Liveness::NotProbed)
        ));

        a.stop().await;
        d.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_declined_offer_is_never_asked_again_while_this_daemon_runs() {
        // D answers no. A records the no verbatim and does not ask again —
        // not on the next ticks, and not on a wake. A person's no is not
        // re-put to them by a loop; a restart may ask once more.
        let root = ScopedRoot::new("declined");
        let net = MemNet::new();
        let a = Side::stand("a", &net, &root, Vec::new()).await;
        let d_id = device_from_seed(&[77; 32]);
        a.own(&[a.me(), d_id.clone()]);
        let (space, _) = a.found("Declined");
        let offers = Arc::new(AtomicUsize::new(0));
        let d_peer: Arc<dyn Transport> = Arc::new(net.peer(d_id.clone()));
        let answerer = {
            let offers = offers.clone();
            let d_peer = d_peer.clone();
            let space = space.clone();
            tokio::spawn(async move {
                while let Some(mut incoming) = d_peer.accept().await {
                    let frame = incoming
                        .stream
                        .recv_bounded(MAX_OWN_FRAME)
                        .await
                        .unwrap()
                        .unwrap();
                    let answer = match postcard::from_bytes::<OwnFrame>(&frame).unwrap() {
                        OwnFrame::Offer { space: offered, .. } => {
                            assert_eq!(offered, space);
                            offers.fetch_add(1, Ordering::SeqCst);
                            postcard::to_stdvec(&OwnAnswer::Declined {
                                why: "not on this box".into(),
                            })
                        }
                        // Where this device is, which is a different
                        // question from whether it wants the Space.
                        OwnFrame::Hello { .. } => {
                            postcard::to_stdvec(&OwnAnswer::Learned { routes: Vec::new() })
                        }
                    }
                    .unwrap();
                    incoming.stream.send(&answer).await.unwrap();
                    incoming.stream.finish().await.unwrap();
                    incoming.stream.wait_closed().await;
                }
            })
        };
        let mut a = a;
        a.start();

        poll_until(Duration::from_secs(30), || async {
            match a.facts.standing(&d_id, &space) {
                Some(FanoutStanding::Declined { why }) => Some(why),
                _ => None,
            }
        })
        .await
        .expect("A recorded the no");
        assert_eq!(
            a.facts.standing(&d_id, &space),
            Some(FanoutStanding::Declined {
                why: "not on this box".into()
            })
        );
        a.own(&[a.me(), d_id.clone()]);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(
            offers.load(Ordering::SeqCst),
            1,
            "a declined offer was put to the device a second time"
        );
        assert!(
            a.reach()
                .await
                .devices
                .iter()
                .find(|row| row.device == d_id.as_str())
                .unwrap()
                .held
                .is_empty(),
            "a no is not a hold"
        );

        answerer.abort();
        a.stop().await;
    }

    /// A retirement is refused before anything is signed when it would empty
    /// an actor: what would be left is a Space nobody could rotate the key of
    /// or admit anyone to again, with the retired machine still able to read
    /// everything already sealed to it.
    #[test]
    fn a_retirement_that_would_leave_a_space_with_no_device_is_refused() {
        let a = device_from_seed(&[1; 32]);
        let d = device_from_seed(&[2; 32]);
        let ledgers = vec![
            ("ws_shared".to_string(), vec![a.clone(), d.clone()]),
            ("ws_theirs".to_string(), vec![d.clone()]),
            ("ws_mine".to_string(), vec![a.clone()]),
        ];
        assert_eq!(
            orphaned_by(&ledgers, &d),
            vec!["ws_theirs".to_string()],
            "only the Space the device is the last of is orphaned by losing it"
        );
        assert_eq!(
            orphaned_by(&ledgers, &a),
            vec!["ws_mine".to_string()],
            "the rule is symmetric: whichever device is the last one is the one that orphans"
        );
        assert!(
            orphaned_by(&ledgers[..1], &d).is_empty(),
            "a Space that keeps a device after the retirement is not orphaned"
        );
        assert!(
            orphaned_by(&ledgers, &device_from_seed(&[3; 32])).is_empty(),
            "a device no ledger names orphans nothing"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_retired_device_is_revoked_in_every_space_it_held() {
        // A holds two Spaces and fans both to D. Retiring D drops it from the
        // profile — the set the hub, this loop and the tunnel all admit on —
        // and then de-lists it in *both* ledgers, one signed op each. Nothing
        // on D is deleted: it keeps every store it entered.
        let root = ScopedRoot::new("retired");
        let net = MemNet::new();
        let a = Side::stand("a", &net, &root, Vec::new()).await;
        let d = Side::stand("d", &net, &root, vec![crate::config::spaces_root()]).await;
        // A really adopts D — the retirement signs against the kinship log,
        // so a set published for the test alone would have nothing to retire.
        a.adopt(&d);
        d.own(&[a.me(), d.me()]);
        let (first, a_first) = a.found("First");
        let (second, a_second) = a.found("Second");
        let mut a = a;
        let mut d = d;
        a.start();
        d.start();

        let d_store = poll_until(Duration::from_secs(60), || async {
            for (space, store) in [(&first, &a_first), (&second, &a_second)] {
                let listed = match a.ask(space, store, Request::DeviceList).await {
                    Response::Text { text } => parse_device_list(&text),
                    _ => Vec::new(),
                };
                if !listed.contains(&d.me()) {
                    return None;
                }
            }
            // And D really holds one of them: the ledger names it because A
            // added it, and the store is what the retirement must not touch.
            bootstrap::registered_home(&d.router, &first)
        })
        .await
        .expect("both Spaces reached D, and D holds the first");
        assert!(d_store.exists());

        let answer = retire(&a.router, &a.facts, d.me().as_str()).await;
        let (revoked_in, unfenced) = match answer {
            Response::Host(crate::control::HostReply::DeviceRetired {
                device,
                revoked_in,
                unfenced,
            }) => {
                assert_eq!(device, d.me().as_str());
                (revoked_in, unfenced)
            }
            other => panic!("the retirement answered {other:?}"),
        };
        let mut revoked = revoked_in;
        revoked.sort();
        let mut both = vec![first.clone(), second.clone()];
        both.sort();
        assert_eq!(revoked, both, "a Space it held was left naming it");
        assert!(
            unfenced.is_empty(),
            "the founder administers both Spaces, so both rotated: {unfenced:?}"
        );

        // The profile first — this is the watch the tunnel drops its routes
        // off, and the hub stops admitting on.
        let set = a
            .router
            .correspondence()
            .own_devices()
            .borrow()
            .clone()
            .expect("held");
        assert!(
            !set.devices.contains(&d.me()),
            "the retired device is still one of the profile's"
        );

        // Then every Space's actor.
        for (space, store) in [(&first, &a_first), (&second, &a_second)] {
            match a.ask(space, store, Request::DeviceList).await {
                Response::Text { text } => assert!(
                    !parse_device_list(&text).contains(&d.me()),
                    "{space} still names the retired device"
                ),
                other => panic!("no device list: {other:?}"),
            }
        }

        // Removal, never deletion: the store D entered is untouched.
        assert!(
            d_store.exists(),
            "retiring a device deleted the store it held"
        );

        let view = a.reach().await;
        assert!(
            !view.devices.iter().any(|row| row.device == d.me().as_str()),
            "the view still draws a device the profile does not name"
        );
        assert!(
            a.facts.standing(&d.me(), &first).is_none(),
            "the memory of asking a retired device outlived it"
        );
        assert!(matches!(
            retire(&a.router, &a.facts, d.me().as_str()).await,
            Response::Error { .. }
        ));
        assert!(
            matches!(retire(&a.router, &a.facts, a.me().as_str()).await, Response::Error { message, .. }
                if message.contains("from another one")),
            "a machine retired itself"
        );

        a.stop().await;
        d.stop().await;
    }

    /// Retired by the other device of the profile, this one keeps its seed and
    /// every store it holds and simply stops speaking on the lane. It answers
    /// no offer — so nothing can consent it back into a Space — and it enters
    /// nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_device_that_is_no_longer_in_the_set_answers_nothing_on_the_lane() {
        let root = ScopedRoot::new("selfretired");
        let net = MemNet::new();
        let a = Side::stand("a", &net, &root, Vec::new()).await;
        let mut d = Side::stand("d", &net, &root, vec![crate::config::spaces_root()]).await;
        link(&[&a, &d]);
        let (space, store) = a.found("Gone");
        let (link_text, actor) = match a.ask(&space, &store, Request::Coordinates).await {
            Response::Coordinates { link, actor, .. } => (link, actor),
            other => panic!("no ticket: {other:?}"),
        };
        d.start();

        // The set as another device of the profile publishes it after the
        // retirement: D is not in it.
        d.own(&[a.me()]);
        let offer = OwnFrame::Offer {
            space: space.clone(),
            actor,
            coordinates: link_text,
            routes: Vec::new(),
        };
        let answered = tokio::time::timeout(
            Duration::from_secs(5),
            exchange(a.transport.as_ref(), &d.me(), &offer),
        )
        .await;
        assert!(
            !matches!(answered, Ok(Ok(OwnAnswer::Consent { .. }))),
            "a retired device consented itself into a Space: {answered:?}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            bootstrap::registered_home(&d.router, &space).is_none(),
            "a retired device entered a Space it was offered"
        );
        // And nothing was taken from it: the seed it signs with is still here.
        assert!(
            d.home.join("secret.key").exists(),
            "a retirement deleted the device's own key"
        );

        d.stop().await;
        a.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_relayed_strangers_ticket_is_refused_by_the_offered_device() {
        // An own device relays a ticket somebody else signed. D refuses
        // before consenting to anything: the ticket's approach Station must
        // be the device offering it, or a stranger's Space would be entered
        // under whatever actor the offer named.
        let root = ScopedRoot::new("relayed");
        let net = MemNet::new();
        let stranger = Side::stand("stranger", &net, &root, Vec::new()).await;
        let mut d = Side::stand("d", &net, &root, vec![crate::config::spaces_root()]).await;
        let a_id = device_from_seed(&[78; 32]);
        d.own(&[d.me(), a_id.clone()]);
        let (space, store) = stranger.found("Theirs");
        let (link, actor) = match stranger.ask(&space, &store, Request::Coordinates).await {
            Response::Coordinates { link, actor, .. } => (link, actor),
            other => panic!("no ticket: {other:?}"),
        };
        d.start();

        let a_peer: Arc<dyn Transport> = Arc::new(net.peer(a_id));
        let offer = OwnFrame::Offer {
            space: space.clone(),
            actor,
            coordinates: link,
            routes: Vec::new(),
        };
        let answer = tokio::time::timeout(
            Duration::from_secs(10),
            exchange(a_peer.as_ref(), &d.me(), &offer),
        )
        .await
        .expect("answered")
        .expect("an answer came back");
        match answer {
            OwnAnswer::Refused { why } => {
                assert_eq!(why, "ticket not signed by the offering device")
            }
            other => panic!("a relayed ticket was answered with {other:?}"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            bootstrap::registered_home(&d.router, &space).is_none(),
            "D entered a Space on a ticket it refused"
        );

        d.stop().await;
        stranger.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_second_offer_of_a_space_already_being_entered_consents_without_entering_twice() {
        // The holder gives up at its answer deadline and offers again while
        // the first entry is still running. Consent is owed both times —
        // the holder asked and this device agrees — but `enter` is
        // idempotent only once the store exists, so the second entry must
        // not start on the same directory.
        let claims = Entering::new();
        let held = claims.claim("ws_one").expect("the first claim is granted");
        assert!(
            claims.claim("ws_one").is_none(),
            "two tasks claimed one Space's entry at the same time"
        );
        assert!(
            claims.claim("ws_two").is_some(),
            "a claim on one Space blocked an unrelated one"
        );
        drop(held);
        assert!(
            claims.claim("ws_one").is_some(),
            "a finished entry did not release its Space"
        );

        let root = ScopedRoot::new("twice");
        let net = MemNet::new();
        let a = Side::stand("a", &net, &root, Vec::new()).await;
        let mut d = Side::stand("d", &net, &root, vec![crate::config::spaces_root()]).await;
        link(&[&a, &d]);
        let (space, store) = a.found("Twice");
        let (link_text, actor) = match a.ask(&space, &store, Request::Coordinates).await {
            Response::Coordinates { link, actor, .. } => (link, actor),
            other => panic!("no ticket: {other:?}"),
        };
        d.start();

        // A's own loop is not running: these are the only two offers made,
        // and the second lands while the first entry is still in flight.
        let offer = OwnFrame::Offer {
            space: space.clone(),
            actor,
            coordinates: link_text,
            routes: Vec::new(),
        };
        for attempt in 1..=2 {
            let answer = tokio::time::timeout(
                Duration::from_secs(10),
                exchange(a.transport.as_ref(), &d.me(), &offer),
            )
            .await
            .expect("answered")
            .expect("an answer came back");
            assert!(
                matches!(answer, OwnAnswer::Consent { .. }),
                "offer {attempt} was answered with {answer:?} rather than a consent"
            );
        }

        d.stop().await;
        a.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_restarted_device_announces_itself_and_the_fan_out_recovers() {
        // The M1 failure, in miniature. B's daemon restarts after pairing
        // and comes back on a new port; under a policy with no relay the
        // route A learned during the ceremony is an address nobody answers
        // on, so every offer fails honestly and forever. B's announcement
        // is what ends that — A takes the routes off the frame and the
        // Space it could not hand over goes across.
        let root = ScopedRoot::new("restart");
        let net = MemNet::new();
        let mut a = Side::stand("a", &net, &root, Vec::new()).await;
        let mut b = Side::stand("b", &net, &root, vec![crate::config::spaces_root()]).await;
        link(&[&a, &b]);
        let stale = StaleRoute::new(a.transport.clone(), b.me());
        a.transport = stale.clone();
        b.transport = Arc::new(Announcing {
            inner: b.transport.clone(),
            addrs: vec!["127.0.0.1:7787".parse().expect("an address")],
        });
        let (space, _) = a.found("Restarted");
        a.start();

        let standing = poll_until(Duration::from_secs(45), || async {
            match a.facts.standing(&b.me(), &space) {
                Some(standing @ FanoutStanding::CouldNotAsk { .. }) => Some(standing),
                Some(other) => panic!("a device with no route was recorded as {other:?}"),
                None => None,
            }
        })
        .await
        .expect("A cannot reach B on the route it holds");
        let FanoutStanding::CouldNotAsk { retry_at_ms, .. } = standing else {
            unreachable!()
        };
        assert!(
            retry_at_ms > crate::daemon::pair::now_ms(),
            "the retry is scheduled, so nothing but an announcement can shorten it"
        );

        // B comes up and says where it is, to the sibling whose address it
        // still knows.
        b.start();
        poll_until(Duration::from_secs(60), || async {
            eprintln!(
                "PROBE stale={} a_standing={:?} b_standing={:?}",
                stale.is_stale(&b.me()),
                a.facts.standing(&b.me(), &space),
                b.facts.standing(&a.me(), &space),
            );
            (a.facts.standing(&b.me(), &space) == Some(FanoutStanding::Held)).then_some(())
        })
        .await
        .expect("the announcement repaired A's route and the Space fanned out");
        assert!(
            !stale.is_stale(&b.me()),
            "A held the Space without ever learning where B is"
        );
        assert_eq!(
            a.facts.liveness_of(&b.me()),
            Liveness::NotProbed,
            "a device that announced itself still reads as one that could not be asked"
        );

        b.stop().await;
        a.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_frame_over_the_lane_bound_is_dropped_unread() {
        // The bound is the lane's, not the sender's: an own device that
        // sent more than `MAX_OWN_FRAME` gets nothing back and reaches no
        // consent. Nothing is owed to a frame that did not arrive whole.
        let root = ScopedRoot::new("oversize");
        let net = MemNet::new();
        let mut d = Side::stand("d", &net, &root, Vec::new()).await;
        let a_id = device_from_seed(&[79; 32]);
        d.own(&[d.me(), a_id.clone()]);
        d.start();

        let a_peer: Arc<dyn Transport> = Arc::new(net.peer(a_id));
        let mut stream = a_peer
            .connect(d.me(), OWN_ALPN)
            .await
            .expect("the lane admits an own device");
        let bloated = postcard::to_stdvec(&OwnFrame::Offer {
            space: "ws_nonsense".into(),
            actor: "act_nonsense".into(),
            coordinates: "x".repeat(MAX_OWN_FRAME + 1),
            routes: Vec::new(),
        })
        .expect("encode");
        assert!(bloated.len() > MAX_OWN_FRAME);
        stream.send(&bloated).await.expect("send");
        let answered =
            tokio::time::timeout(Duration::from_secs(3), stream.recv_bounded(MAX_OWN_FRAME)).await;
        assert!(
            !matches!(answered, Ok(Ok(Some(_)))),
            "an oversized frame was answered: {answered:?}"
        );

        d.stop().await;
    }
}
