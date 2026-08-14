//! Peer presence, measured — never assumed.
//!
//! Sampled passively: every Orbit this identity serves is asked over
//! `request_if_running`, so reading presence never places a vacant Orbit. A
//! Space that is not running, or could not be asked, contributes nothing, and
//! the map records which Spaces *did* answer — so a consumer can tell
//! "offline" from "could not be asked". Those are different facts, and only
//! one of them is worth acting on.
//!
//! The measurement itself is the engine's: the Neighbor registry's advisory
//! reachability (verified Beacons, swarm membership, Contact outcomes),
//! projected per device with the actor each device speaks for resolved
//! through the Station's authority view. Nothing here invents a state — a
//! peer this device has never seen simply is not in the map.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lait::control::{ControlRoute, OrbitAddress, Request, Response};

use super::host::OrbitEntry;
use super::Client;

/// Reachability as the Neighbor registry measures it, from this device's
/// vantage. Ordered worst-to-best so joining several observations of one
/// identity can take the best: a person online on one device is online.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    Offline,
    Away,
    Online,
}

impl Reach {
    fn parse(state: &str) -> Self {
        match state {
            "online" => Self::Online,
            "away" => Self::Away,
            _ => Self::Offline,
        }
    }
}

/// What one sampling pass learned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresenceMap {
    /// Space ids that answered. A handle scoped to a Space absent from this
    /// set was never measured — that absence is "could not be asked", and it
    /// must never collapse into "offline".
    pub asked: BTreeSet<String>,
    /// Best observed reach per `(space, actor)`.
    pub actors: BTreeMap<(String, String), Reach>,
    /// Best observed reach per device id, across every Space that answered.
    pub devices: BTreeMap<String, Reach>,
}

impl PresenceMap {
    fn absorb(&mut self, space: &str, peers: Vec<lait::control::PresenceEntry>) {
        self.asked.insert(space.to_owned());
        for peer in peers {
            let reach = Reach::parse(&peer.state);
            merge(&mut self.devices, peer.id, reach);
            if let Some(actor) = peer.actor {
                merge_keyed(&mut self.actors, (space.to_owned(), actor), reach);
            }
        }
    }
}

fn merge(map: &mut BTreeMap<String, Reach>, key: String, reach: Reach) {
    map.entry(key)
        .and_modify(|held| *held = (*held).max(reach))
        .or_insert(reach);
}

fn merge_keyed(map: &mut BTreeMap<(String, String), Reach>, key: (String, String), reach: Reach) {
    map.entry(key)
        .and_modify(|held| *held = (*held).max(reach))
        .or_insert(reach);
}

impl Client {
    /// Ask every running Space who is around. Never places: a vacant Orbit,
    /// an unreachable daemon, or a refused read all leave that Space out of
    /// `asked`, which downstream reads as unmeasured rather than as anything
    /// about the peers.
    pub async fn presence(&self, orbits: &[OrbitEntry]) -> PresenceMap {
        let mut map = PresenceMap::default();
        let Ok(daemon) = self.daemon() else {
            return map;
        };
        for orbit in orbits {
            let Some(space) = mechanics::ids::SpaceId::parse(&orbit.space) else {
                continue;
            };
            let route = ControlRoute::Orbit {
                address: OrbitAddress::for_store(Path::new(&orbit.path), space),
            };
            match daemon.request_if_running(route, &Request::Who).await {
                Ok(Response::Who { peers }) => map.absorb(&orbit.space, peers),
                // Not running, not reachable, or refused: nothing was
                // measured, and nothing is recorded — absence, of the
                // "could not be asked" kind.
                _ => {}
            }
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, actor: Option<&str>, state: &str) -> lait::control::PresenceEntry {
        lait::control::PresenceEntry {
            id: id.to_owned(),
            nick: String::new(),
            actor: actor.map(str::to_owned),
            state: state.to_owned(),
            online: state == "online",
            last_seen_secs: 0,
            dialable: true,
            blocked_by: None,
            pending: false,
            due_in_secs: 0,
            route_lease_secs: 0,
            failures: 0,
        }
    }

    /// One person on two devices is as reachable as their most reachable
    /// device. An average would say a person half-here, which is not a thing.
    #[test]
    fn the_best_observation_of_an_identity_wins() {
        let mut map = PresenceMap::default();
        map.absorb(
            "ws_one",
            vec![
                entry("dev_a", Some("act_ada"), "offline"),
                entry("dev_b", Some("act_ada"), "online"),
            ],
        );
        assert_eq!(
            map.actors.get(&("ws_one".to_owned(), "act_ada".to_owned())),
            Some(&Reach::Online)
        );
    }

    /// A row whose Station resolves no actor still measures the device — the
    /// person is unknown, not invented from the device id.
    #[test]
    fn an_unresolved_station_measures_only_its_device() {
        let mut map = PresenceMap::default();
        map.absorb("ws_one", vec![entry("dev_a", None, "away")]);
        assert!(map.actors.is_empty());
        assert_eq!(map.devices.get("dev_a"), Some(&Reach::Away));
    }

    /// The map says which Spaces answered. That set is what keeps "offline"
    /// (asked, nobody speaks for them) apart from "could not be asked".
    #[test]
    fn an_unasked_space_is_recorded_as_unasked() {
        let map = PresenceMap::default();
        assert!(!map.asked.contains("ws_one"));
        let mut asked = PresenceMap::default();
        asked.absorb("ws_one", Vec::new());
        assert!(asked.asked.contains("ws_one"));
    }
}
