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
use tokio::sync::{watch, Notify, Semaphore};

use crate::control::{
    ControlRoute, DeviceStanding, FanoutStanding, Liveness, Request, Response, SpaceFanout,
};
use crate::daemon::correspondence::OwnDevices;
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
/// How long an answer stands before the device is asked again. An answer is
/// not a fact about the Space — only the ledger is — so it is remembered,
/// not kept.
const STANDING_LIFETIME: Duration = Duration::from_secs(3600);
/// How many Spaces an offered device enters at once. Each entry drives
/// admission for up to thirty seconds; a new device receiving every Space
/// must not serialize them, and must not place all of them either.
const ENTER_CONCURRENCY: usize = 2;
/// The bounded wait for this identity's routes before an offer is sent.
const ROUTES_DEADLINE: Duration = Duration::from_secs(3);

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
    /// "What do you hold?"
    Probe { routes: Vec<SocketAddr> },
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
    Report(Report),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Report {
    pub version: String,
    pub excluded: Vec<String>,
    pub spaces: Vec<(String, Membership)>,
    pub routes: Vec<SocketAddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Membership {
    Member,
    Pending,
    Absent,
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

    /// Record that `device` did not answer, with the next time to try.
    fn could_not_ask(&self, device: &DeviceId, space: &str, why: String, now_ms: u64) {
        let tries = self
            .standings
            .lock_recovering()
            .get(&(device.clone(), space.to_string()))
            .map_or(0, |r| r.tries);
        let base = RETRY_FLOOR
            .checked_mul(1u32 << tries.min(16))
            .unwrap_or(RETRY_CEILING)
            .min(RETRY_CEILING);
        let delay = crate::update::watch::next_delay(base, RETRY_SPREAD);
        let retry_at_ms =
            now_ms.saturating_add(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
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
    fn note_ledger(&self, space: &str, devices: Vec<DeviceId>, now_ms: u64) {
        {
            let mut standings = self.standings.lock_recovering();
            standings.retain(|(device, s), recorded| {
                s != space
                    || devices.contains(device)
                    || !matches!(recorded.standing, FanoutStanding::Held)
            });
        }
        for device in &devices {
            self.record(device, space, FanoutStanding::Held, now_ms);
        }
        self.ledger
            .lock_recovering()
            .insert(space.to_string(), devices);
    }

    /// Whether `(device, space)` is worth asking now. Nothing recorded is
    /// due; `Held` never is; a device that could not be asked is due at its
    /// retry time, or at once when woken; an answer is due again only after
    /// it has stood its lifetime — a wake does not re-ask a no.
    fn due(&self, device: &DeviceId, space: &str, now_ms: u64, woken: bool) -> bool {
        let recorded = self
            .standings
            .lock_recovering()
            .get(&(device.clone(), space.to_string()))
            .cloned();
        let Some(recorded) = recorded else {
            return true;
        };
        let lifetime = u64::try_from(STANDING_LIFETIME.as_millis()).unwrap_or(u64::MAX);
        let floor = u64::try_from(RETRY_FLOOR.as_millis()).unwrap_or(u64::MAX);
        match &recorded.standing {
            FanoutStanding::Held => false,
            FanoutStanding::CouldNotAsk { retry_at_ms, .. } => woken || now_ms >= *retry_at_ms,
            FanoutStanding::Deferred { .. } => {
                woken || now_ms >= recorded.at_ms.saturating_add(floor)
            }
            FanoutStanding::Declined { .. } | FanoutStanding::Refused { .. } => {
                now_ms >= recorded.at_ms.saturating_add(lifetime)
            }
        }
    }
}

/// The fan-out, both sides: the holder's reconcile loop, and the answerer
/// on the Own lane. Runs until `stop`; `wake` skips the current backoff
/// once. `own` is `None` until the set is restored, and then nothing is
/// offered or answered — the hub admits nobody on the lane either.
pub(crate) async fn serve(
    router: Arc<Router>,
    transport: Arc<dyn Transport>,
    mut own: watch::Receiver<Option<OwnDevices>>,
    facts: Arc<Facts>,
    wake: Arc<Notify>,
    mut stop: watch::Receiver<bool>,
) {
    let answerer = tokio::spawn(answer_loop(router.clone(), transport.clone(), stop.clone()));
    let mut doorbells = router.subscribe();
    let mut bells_open = true;
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut woken = false;
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            () = wake.notified() => woken = true,
            changed = own.changed() => {
                if changed.is_err() {
                    break;
                }
                // A set that just changed is worth asking about now.
                woken = true;
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
                let skip_backoff = std::mem::take(&mut woken);
                step(&router, transport.as_ref(), &set, &facts, skip_backoff).await;
            }
        }
    }
    if let Err(error) = tokio::time::timeout(Duration::from_secs(5), answerer).await {
        tracing::debug!(%error, "the Own lane answerer did not finish in time");
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
            facts.note_ledger(space, devices.clone(), crate::daemon::pair::now_ms());
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
        OwnAnswer::Report(report) => {
            facts.liveness.lock_recovering().insert(
                device.clone(),
                Liveness::Reported {
                    version: report.version,
                    at: now,
                },
            );
            FanoutStanding::Deferred {
                why: "answered an offer with a report".into(),
            }
        }
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

/// The other side of the lane. Everything that arrives here was admitted
/// by the hub against the device set; what is checked here is the offer
/// itself — that the ticket was signed by the device offering it, for the
/// Space it says.
async fn answer_loop(
    router: Arc<Router>,
    transport: Arc<dyn Transport>,
    mut stop: watch::Receiver<bool>,
) {
    let entering = Arc::new(Semaphore::new(ENTER_CONCURRENCY));
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
    entering: Arc<Semaphore>,
    mut incoming: Incoming,
) {
    let from = incoming.from.clone();
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
    let answer = match frame {
        OwnFrame::Offer {
            space,
            actor,
            coordinates,
            routes,
        } => {
            transport.learn(from.clone(), &routes);
            answer_offer(router, &from, &space, &actor, coordinates, entering).await
        }
        OwnFrame::Probe { routes } => {
            transport.learn(from.clone(), &routes);
            report(&router, transport.as_ref()).await
        }
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

fn is_member(membership: &str) -> bool {
    matches!(membership, "admin" | "member")
}

async fn answer_offer(
    router: Arc<Router>,
    from: &DeviceId,
    space: &str,
    actor: &str,
    coordinates: String,
    entering: Arc<Semaphore>,
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
    // Already held here as a member: nothing to consent to. Only a store
    // this machine registered is asked, and asking places it.
    let registered = bootstrap::registered_home(&router, space);
    if let Some(home) = &registered {
        let route = ControlRoute::Orbit {
            address: OrbitAddress::for_store(home, verified.space.clone()),
        };
        if let Ok(Response::Status(info)) =
            router.request_routed(route, &Request::Status, None).await
        {
            if is_member(&info.membership) {
                return OwnAnswer::Held;
            }
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
    tokio::spawn(async move {
        let Ok(_permit) = entering.acquire_owned().await else {
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

/// What this device holds, for a probe. A Space whose Station could not be
/// asked is left out and said so in the log — never reported as absent.
async fn report(router: &Router, transport: &dyn Transport) -> OwnAnswer {
    let mut spaces = Vec::new();
    for (space, path) in own_spaces(router) {
        let Some(route) = route_for(&space, &path) else {
            continue;
        };
        match router.request_routed(route, &Request::Status, None).await {
            Ok(Response::Status(info)) => spaces.push((
                space,
                if is_member(&info.membership) {
                    Membership::Member
                } else {
                    Membership::Pending
                },
            )),
            Ok(_) | Err(_) => {
                tracing::debug!(space, "this Space could not be asked for a report");
            }
        }
    }
    let routes = transport
        .advertised_routes(ROUTES_DEADLINE)
        .await
        .unwrap_or_default();
    OwnAnswer::Report(Report {
        version: crate::VERSION.to_string(),
        excluded: Vec::new(),
        spaces,
        routes,
    })
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

    /// One daemon's worth of fan-out: an identity home, a router over the
    /// shared network whose catalog sees only the stores under `roots`, the
    /// identity transport, and the loop once started.
    struct Side {
        home: PathBuf,
        seed: [u8; 32],
        router: Arc<Router>,
        transport: Arc<dyn Transport>,
        facts: Arc<Facts>,
        wake: Arc<Notify>,
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
            let catalog = crate::orbits::Catalog::with_loader(
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
                wake: Arc::new(Notify::new()),
                stop: watch::Sender::new(false),
                task: None,
            }
        }

        fn me(&self) -> DeviceId {
            device_from_seed(&self.seed)
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
                self.wake.clone(),
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
                Response::Status(info) if is_member(&info.membership) => Some(store),
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
        a.wake.notify_one();
        // Well inside the thirty-second floor the retry was scheduled at.
        poll_until(Duration::from_secs(20), || async {
            (a.facts.standing(&d.me(), &space) == Some(FanoutStanding::Held)).then_some(())
        })
        .await
        .expect("healed and woken, the Space is held on D before the backoff would have run");

        a.stop().await;
        d.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_declined_offer_is_not_retried_until_the_standing_expires() {
        // D answers no. A records the no verbatim and does not ask again on
        // the next ticks — nor on a wake, which only skips a backoff.
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
                    match postcard::from_bytes::<OwnFrame>(&frame).unwrap() {
                        OwnFrame::Offer { space: offered, .. } => assert_eq!(offered, space),
                        other => panic!("not an offer: {other:?}"),
                    }
                    offers.fetch_add(1, Ordering::SeqCst);
                    let answer = postcard::to_stdvec(&OwnAnswer::Declined {
                        why: "not on this box".into(),
                    })
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
        a.wake.notify_one();
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(
            offers.load(Ordering::SeqCst),
            1,
            "a declined offer was asked again before its standing expired"
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
}
