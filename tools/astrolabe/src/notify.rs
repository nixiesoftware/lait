//! What the client tells somebody about when no window is open.
//!
//! The client is the only thing that sees every Orbit at once, and the only
//! thing still running when every window is closed. That makes cross-Space
//! notification a client capability and nothing else's: a page scoped to one
//! Space cannot see the others, and cannot fire when it is closed.
//!
//! ## What is notifiable today, and what is not
//!
//! Device and peer news is derived from *two authoritative readings* — the
//! snapshot that just arrived and the one before it. Nothing is invented:
//! a peer that appears in one reading and not the previous one arrived, and
//! that is a fact about what was observed rather than an inference about
//! what happened.
//!
//! Sponsorship asks are a third reading, from the host plane. They are
//! *decisions waiting*, not ambient topology: the first time this client sees
//! one is news, even on a cold start, because nobody has answered it yet.
//! They are still a diff of two lists — not a World signal. Item-level
//! notification (an assignment, a comment, a mention) stays a World's
//! vocabulary. The substrate has a product-neutral channel for exactly that
//! (`Request::Signals`), but reading it is a *drain*: it empties the queue,
//! and `src/serve/socket.rs` already drains it for whatever browser is
//! attached. A second consumer would take signals the page never sees. That
//! needs a per-consumer cursor before either is honest, and it is filed
//! rather than worked around.
//!
//! ## Muting is honestly global
//!
//! A muted client does not notify, rather than notifying quietly. There is no
//! "important enough to override quiet" tier, because every implementation of
//! that tier is somebody deciding on your behalf which of your evenings gets
//! interrupted.

use std::collections::{BTreeMap, BTreeSet};

use lait_workbench::{ConnectionSnapshot, DeviceSnapshot, LifecycleState, ObservationState};

/// Something worth telling a person who is not looking at the window.
///
/// Named for what it costs rather than for what it carries: every one of these
/// takes somebody's attention away from whatever they were doing, and a type
/// called `Notice` invites a list that grows until nobody reads any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interruption {
    /// A daemon this client manages stopped in a way nobody asked for.
    DeviceFailed { device: String, why: String },
    /// A device's figures stopped being current.
    ObservationDegraded { device: String, why: String },
    /// A peer came online in a Space.
    PeerArrived { space: String, peer: String },
    /// A peer this device could see stopped being visible.
    PeerLeft { space: String, peer: String },
    /// A co-located agent attached without standing and is waiting on a person.
    SponsorshipAsked { space: String, agent: String },
}

impl Interruption {
    /// The Space this is about, when it is about one.
    ///
    /// `None` is a fact about the machine rather than about a Space — a daemon
    /// that failed is not something a per-Space mute can be about, and folding
    /// it into one would let muting a Space silence a failure that has nothing
    /// to do with it.
    pub fn space(&self) -> Option<&str> {
        match self {
            Self::DeviceFailed { .. } | Self::ObservationDegraded { .. } => None,
            Self::PeerArrived { space, .. }
            | Self::PeerLeft { space, .. }
            | Self::SponsorshipAsked { space, .. } => Some(space),
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::DeviceFailed { device, .. } => format!("{device} stopped"),
            Self::ObservationDegraded { device, .. } => format!("{device} is out of date"),
            Self::PeerArrived { peer, .. } => format!("{peer} is here"),
            Self::PeerLeft { peer, .. } => format!("{peer} went quiet"),
            Self::SponsorshipAsked { agent, .. } => format!("{agent} wants to be sponsored"),
        }
    }

    pub fn body(&self) -> String {
        match self {
            Self::DeviceFailed { why, .. } | Self::ObservationDegraded { why, .. } => why.clone(),
            Self::PeerArrived { space, .. }
            | Self::PeerLeft { space, .. }
            | Self::SponsorshipAsked { space, .. } => {
                format!("in {space}")
            }
        }
    }
}

/// Who is allowed to interrupt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quiet {
    /// Nothing notifies. Global means global.
    pub everything: bool,
    /// Spaces that do not notify.
    pub spaces: BTreeSet<String>,
}

impl Quiet {
    /// Whether this may be said out loud.
    pub fn permits(&self, notice: &Interruption) -> bool {
        if self.everything {
            return false;
        }
        notice
            .space()
            .is_none_or(|space| !self.spaces.contains(space))
    }

    pub fn mute(&mut self, space: &str, muted: bool) {
        if muted {
            self.spaces.insert(space.to_owned());
        } else {
            self.spaces.remove(space);
        }
    }

    pub fn is_muted(&self, space: &str) -> bool {
        self.spaces.contains(space)
    }
}

/// What changed between two authoritative readings.
///
/// A pure function of the two, which is what makes every rule here testable
/// without a daemon, a window or a clock. The first reading produces nothing:
/// arriving at a machine with four peers already online is not four people
/// turning up.
pub fn between(
    previous: Option<(&[DeviceSnapshot], &[ConnectionSnapshot])>,
    current: (&[DeviceSnapshot], &[ConnectionSnapshot]),
) -> Vec<Interruption> {
    let Some((was_devices, was_connections)) = previous else {
        return Vec::new();
    };
    let (devices, connections) = current;
    let mut notices = Vec::new();

    let before: BTreeMap<&str, &DeviceSnapshot> = was_devices
        .iter()
        .map(|device| (device.id.as_str(), device))
        .collect();
    for device in devices {
        let Some(was) = before.get(device.id.as_str()) else {
            // A device that did not exist a moment ago was just registered, and
            // registering it is what the person was doing. Not news.
            continue;
        };
        if device.state == LifecycleState::Failed && was.state != LifecycleState::Failed {
            notices.push(Interruption::DeviceFailed {
                device: device.label.clone(),
                why: device
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "it stopped without saying why".to_owned()),
            });
        }
        if device.observation.state == ObservationState::Degraded
            && was.observation.state != ObservationState::Degraded
        {
            notices.push(Interruption::ObservationDegraded {
                device: device.label.clone(),
                why: device
                    .observation
                    .error
                    .clone()
                    .unwrap_or_else(|| "it could not be sampled".to_owned()),
            });
        }
    }

    let online = |connections: &[ConnectionSnapshot]| -> BTreeSet<(String, String)> {
        connections
            .iter()
            .filter(|connection| connection.online)
            .map(|connection| (connection.space_id.clone(), connection.peer_nick.clone()))
            .collect()
    };
    let was_online = online(was_connections);
    let is_online = online(connections);
    for (space, peer) in is_online.difference(&was_online) {
        notices.push(Interruption::PeerArrived {
            space: space.clone(),
            peer: peer.clone(),
        });
    }
    for (space, peer) in was_online.difference(&is_online) {
        notices.push(Interruption::PeerLeft {
            space: space.clone(),
            peer: peer.clone(),
        });
    }

    notices
}

/// What changed between two readings of the host-plane ask list.
///
/// Unlike [`between`], the first reading *is* news: an unanswered ask is a
/// decision waiting, not four peers who were already online when the client
/// started. Later readings only say what appeared.
pub fn asks_between(
    previous: Option<&BTreeSet<(String, String)>>,
    current: &BTreeSet<(String, String)>,
) -> Vec<Interruption> {
    let fresh: Vec<&(String, String)> = match previous {
        None => current.iter().collect(),
        Some(was) => current.difference(was).collect(),
    };
    fresh
        .into_iter()
        .map(|(space, agent)| Interruption::SponsorshipAsked {
            space: space.clone(),
            agent: agent.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lait_workbench::ObservationHealth;

    fn device(id: &str, state: LifecycleState, observation: ObservationState) -> DeviceSnapshot {
        DeviceSnapshot {
            id: id.into(),
            label: id.into(),
            home: "home".into(),
            log_path: "log".into(),
            state,
            pid: None,
            owned: true,
            started_at_ms: None,
            last_error: None,
            facts: None,
            observation: ObservationHealth {
                state: observation,
                sampled_at_ms: None,
                stale_since_ms: None,
                error: Some("control channel refused".into()),
            },
            image: None,
        }
    }

    fn peer(space: &str, nick: &str, online: bool) -> ConnectionSnapshot {
        ConnectionSnapshot {
            source_device_id: "alice".into(),
            space_id: space.into(),
            peer_id: nick.into(),
            peer_nick: nick.into(),
            state: "connected".into(),
            online,
            dialable: true,
            blocked_by: None,
            target_device_id: None,
        }
    }

    /// The first reading is not news. Arriving at a machine with four peers
    /// already online is not four people turning up, and a client that said so
    /// would interrupt somebody every time it started.
    #[test]
    fn the_first_reading_of_a_machine_notifies_nothing() {
        let devices = vec![device(
            "alice",
            LifecycleState::Failed,
            ObservationState::Degraded,
        )];
        let peers = vec![peer("ws_one", "bob", true)];
        assert!(between(None, (&devices, &peers)).is_empty());
    }

    /// A transition is news; the state it transitioned into is not. Otherwise
    /// every reading of a machine with one failed device is another interruption
    /// about the same failure.
    #[test]
    fn only_the_moment_something_changed_is_worth_saying() {
        let running = vec![device(
            "alice",
            LifecycleState::Running,
            ObservationState::Healthy,
        )];
        let failed = vec![device(
            "alice",
            LifecycleState::Failed,
            ObservationState::Healthy,
        )];

        let first = between(Some((&running, &[])), (&failed, &[]));
        assert_eq!(first.len(), 1);
        assert!(matches!(
            first.first(),
            Some(Interruption::DeviceFailed { .. })
        ));

        assert!(
            between(Some((&failed, &[])), (&failed, &[])).is_empty(),
            "a device that was already failed was reported as having just failed"
        );
    }

    /// A degraded observation is worth saying once, and it says why. "Something
    /// is stale" with no reason is not something a person can act on.
    #[test]
    fn a_sampling_failure_is_reported_with_its_reason() {
        let fresh = vec![device(
            "alice",
            LifecycleState::Running,
            ObservationState::Healthy,
        )];
        let stale = vec![device(
            "alice",
            LifecycleState::Running,
            ObservationState::Degraded,
        )];
        let notices = between(Some((&fresh, &[])), (&stale, &[]));
        assert_eq!(notices.len(), 1);
        let notice = notices.first().expect("a notice");
        assert!(
            notice.body().contains("control channel refused"),
            "{}",
            notice.body()
        );
    }

    /// Arrival and departure are both observed, and both name the Space they
    /// happened in — which is what makes a per-Space mute mean anything.
    #[test]
    fn a_peer_arriving_and_leaving_are_both_said_and_both_name_their_space() {
        let none = vec![peer("ws_one", "bob", false)];
        let here = vec![peer("ws_one", "bob", true)];

        let arrived = between(Some((&[], &none)), (&[], &here));
        assert_eq!(
            arrived,
            vec![Interruption::PeerArrived {
                space: "ws_one".into(),
                peer: "bob".into()
            }]
        );
        let left = between(Some((&[], &here)), (&[], &none));
        assert_eq!(
            left,
            vec![Interruption::PeerLeft {
                space: "ws_one".into(),
                peer: "bob".into()
            }]
        );
    }

    /// Muting one Space silences that Space and nothing else — and never
    /// silences a fact about the machine, which is not a Space's to mute.
    #[test]
    fn muting_a_space_silences_that_space_alone() {
        let mut quiet = Quiet::default();
        quiet.mute("ws_one", true);

        assert!(!quiet.permits(&Interruption::PeerArrived {
            space: "ws_one".into(),
            peer: "bob".into()
        }));
        assert!(quiet.permits(&Interruption::PeerArrived {
            space: "ws_two".into(),
            peer: "bob".into()
        }));
        assert!(
            quiet.permits(&Interruption::DeviceFailed {
                device: "alice".into(),
                why: "gone".into()
            }),
            "muting a Space silenced a failure that had nothing to do with it"
        );
        assert!(
            !quiet.permits(&Interruption::SponsorshipAsked {
                space: "ws_one".into(),
                agent: "grok".into()
            }),
            "muting a Space did not silence a sponsorship ask in it"
        );

        quiet.mute("ws_one", false);
        assert!(quiet.permits(&Interruption::PeerArrived {
            space: "ws_one".into(),
            peer: "bob".into()
        }));
    }

    /// An unanswered ask is news the first time this client sees it. Arriving
    /// at a machine with a decision waiting is not the same as arriving at
    /// one with four peers already online.
    #[test]
    fn the_first_reading_of_a_pending_ask_is_worth_saying() {
        let current = BTreeSet::from([("ws_one".into(), "grok".into())]);
        assert_eq!(
            asks_between(None, &current),
            vec![Interruption::SponsorshipAsked {
                space: "ws_one".into(),
                agent: "grok".into()
            }]
        );
        assert!(
            asks_between(Some(&current), &current).is_empty(),
            "a standing ask was reported as having just appeared"
        );
    }

    #[test]
    fn only_a_new_ask_is_said_the_second_time() {
        let was = BTreeSet::from([("ws_one".into(), "grok".into())]);
        let now = BTreeSet::from([
            ("ws_one".into(), "grok".into()),
            ("ws_one".into(), "claude".into()),
        ]);
        assert_eq!(
            asks_between(Some(&was), &now),
            vec![Interruption::SponsorshipAsked {
                space: "ws_one".into(),
                agent: "claude".into()
            }]
        );
    }

    /// Global means global. There is no tier important enough to override it,
    /// because every implementation of that tier is somebody deciding on your
    /// behalf which of your evenings gets interrupted.
    #[test]
    fn a_quiet_client_says_nothing_at_all() {
        let quiet = Quiet {
            everything: true,
            spaces: BTreeSet::new(),
        };
        for notice in [
            Interruption::DeviceFailed {
                device: "alice".into(),
                why: "gone".into(),
            },
            Interruption::ObservationDegraded {
                device: "alice".into(),
                why: "stale".into(),
            },
            Interruption::PeerArrived {
                space: "ws_one".into(),
                peer: "bob".into(),
            },
            Interruption::SponsorshipAsked {
                space: "ws_one".into(),
                agent: "grok".into(),
            },
        ] {
            assert!(
                !quiet.permits(&notice),
                "a globally quiet client still said {notice:?}"
            );
        }
    }
}
