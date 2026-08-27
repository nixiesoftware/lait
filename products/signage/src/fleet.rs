//! The fleet: panels, the channels they are tuned to, and the broadcasts that
//! interrupt them.
//!
//! The television split, because it is the one operators already own and
//! because it separates two things this product had welded together. A
//! *program* is authored content. A *channel* is what a screen is tuned to. A
//! *broadcast* is a transmission to an audience over a window. Wiring a
//! playlist directly onto a panel — which is what a screen's `intent` used to
//! be — is why re-addressing a fleet was expensive: the wire ran from the
//! content to the glass, so every change had to be made once per pane.
//!
//! Resolution is a pure function of a screen, the documents that address it,
//! and a [`Context`]. Nothing here reads a clock or a sensor; both arrive in
//! the context, which is what lets two replicas agree on what a screen shows.

use replica::body::{BodyId, BodyKey};
use serde::{Deserialize, Serialize};

use crate::addressing::{AudienceLookup, Context, Match, Place};
use crate::contract::{
    body_key, valid_kind, valid_settings, Boundary, Settings, MAX_CHANNEL_WINDOWS, MAX_NAME_CHARS,
    MAX_SCREEN_LABELS, MAX_SUPERSEDES,
};

/// One panel. Where it is, what it is called, what it is tuned to.
///
/// Intent is replicated; the grant that lets a receiver fetch stays with the
/// coordinator. A row carrying both would make revocation a thing two planes
/// disagreed about.
///
/// The fields divide on one line: **facts are true of the panel, labels are
/// what somebody decided to call it.** A screen is in exactly one physical
/// place because that is how places work, and in as many labels as its owner
/// finds useful because that is how thinking works. Welding those together —
/// one `group`, carrying both the venue and the organisation — is what forced
/// every operator to choose a single axis and stay on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignageScreen {
    pub id: String,
    pub name: String,

    /// Geography. `None` is a screen nobody has sited yet, which is a state to
    /// draw rather than a default to invent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<Place>,

    /// What each kind knows about this venue, in that kind's own vocabulary:
    /// `athan` stores its method, its school, its iqamah offsets. Untyped for
    /// the same reason settings are — the substrate does not learn anybody's
    /// jurisprudence.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub facts: std::collections::BTreeMap<String, Settings>,

    /// A frame-lock cohort. Single-valued because a panel can only be locked
    /// to one wall — a fact about the installation, not an organisation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<String>,

    /// The operator's own vocabulary. Overlapping and arbitrary on purpose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,

    /// What it shows when nothing is being broadcast at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuned: Option<String>,
}

impl SignageScreen {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && self.place.as_ref().is_none_or(Place::validate)
            && self
                .tuned
                .as_ref()
                .is_none_or(|channel| BodyId::parse(channel).is_some())
            && self
                .sync
                .as_ref()
                .is_none_or(|cohort| BodyId::parse(cohort).is_some())
            && self.labels.len() <= MAX_SCREEN_LABELS
            && self
                .labels
                .iter()
                .all(|label| crate::addressing::valid_label(label))
            && self.labels.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .facts
                .iter()
                .all(|(kind, settings)| valid_kind(kind) && valid_settings(settings))
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

/// A standing stream a screen tunes to.
///
/// It has its own dayparts, because "breakfast until eleven, then lunch" is a
/// property of the channel rather than an interruption of it. A broadcast is
/// what interrupts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignageChannel {
    pub id: String,
    pub name: String,
    /// What plays when no window of its own is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedule: Vec<ChannelWindow>,
}

/// Which program a channel carries, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelWindow {
    pub id: String,
    #[serde(flatten)]
    pub window: schedule::Window,
    pub program: String,
}

impl SignageChannel {
    pub fn validate(&self) -> bool {
        if BodyId::parse(&self.id).is_none()
            || self.name.trim().is_empty()
            || self.name.chars().count() > MAX_NAME_CHARS
            || self.schedule.len() > MAX_CHANNEL_WINDOWS
            || !self
                .base
                .as_ref()
                .is_none_or(|program| BodyId::parse(program).is_some())
        {
            return false;
        }
        let mut seen = std::collections::BTreeSet::new();
        self.schedule.iter().all(|window| {
            BodyId::parse(&window.program).is_some()
                && !window.id.is_empty()
                && window.id.len() <= MAX_NAME_CHARS
                && window.window.validate().is_ok()
                && seen.insert(&window.id)
        })
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

/// What a broadcast does to the screens it reaches.
///
/// Open, not closed. `Kind` carries an action this World does not interpret,
/// the way `Settings` carries values it does not interpret — so a capability
/// an app invents does not require a contract change to address. A closed set
/// here would mean every new thing a screen can be told needs the substrate's
/// permission first, which is the shape that makes a platform stale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Play {
        program: String,
    },
    /// Treat the reached screens as tuned elsewhere while this is open. A
    /// redirection, never a write — resolution stays a read.
    Tune {
        channel: String,
    },
    /// Deliberately dark, which is not the same as unaddressed.
    Blank,
    /// The all-clear: fall through to the channel, outranking anything below.
    Restore,
    Kind {
        kind: String,
        settings: Settings,
    },
}

impl Action {
    fn validate(&self) -> bool {
        match self {
            Self::Play { program } => BodyId::parse(program).is_some(),
            Self::Tune { channel } => BodyId::parse(channel).is_some(),
            Self::Blank | Self::Restore => true,
            Self::Kind { kind, settings } => valid_kind(kind) && valid_settings(settings),
        }
    }
}

/// When a broadcast is open.
///
/// A window is the common case and stays first-class. `When` is the same
/// predicate language the audience is written in, evaluated against the same
/// context — so "at eleven" and "while the queue is long" are the same kind of
/// statement rather than one being a feature and the other a rewrite.
///
/// A third arm belongs here eventually: a *goal* — "about four times an hour
/// through August" — which is stage one of Clinch et al.'s model and the thing
/// every scheduler grows. It is not built. The enum is shaped so it arrives
/// without disturbing anything else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "timing", rename_all = "snake_case")]
pub enum Timing {
    Window {
        #[serde(flatten)]
        window: schedule::Window,
    },
    When {
        of: Match,
        priority: i16,
    },
}

impl Timing {
    /// One source of truth per arm, rather than a priority field beside a
    /// window that already has one.
    pub fn priority(&self) -> i16 {
        match self {
            Self::Window { window } => window.priority,
            Self::When { priority, .. } => *priority,
        }
    }

    fn validate(&self) -> bool {
        match self {
            Self::Window { window } => window.validate().is_ok(),
            Self::When { of, .. } => of.validate(),
        }
    }
}

/// A transmission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignageBroadcast {
    pub id: String,
    pub name: String,
    /// The audience it reaches, by reference.
    pub audience: String,
    pub action: Action,
    pub timing: Timing,
    /// Broadcasts this one replaces. Lifted from CAP's `references`: an
    /// all-clear has to travel faster than an expiry, and "cancel that one"
    /// has to survive the cancelling client going away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at_unix_ms: Option<u64>,
}

impl SignageBroadcast {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && BodyId::parse(&self.audience).is_some()
            && self.action.validate()
            && self.timing.validate()
            && self.supersedes.len() <= MAX_SUPERSEDES
            && self
                .supersedes
                .iter()
                .all(|id| BodyId::parse(id).is_some() && id != &self.id)
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }

    fn cancelled_by(&self, now_unix_ms: u64) -> bool {
        self.cancelled_at_unix_ms
            .is_some_and(|at| now_unix_ms >= at)
    }
}

/// What a screen is showing.
///
/// Three states, not two. `Blank` is a screen told to go dark; `Unaddressed`
/// is a screen nothing reaches. Folding them together is the defect this
/// codebase names everywhere else — unmeasured is absent, never zero — and the
/// difference is exactly what an operator needs when a panel is showing
/// nothing and they are trying to find out why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "showing", rename_all = "snake_case")]
pub enum Showing {
    Program {
        program: String,
    },
    Blank,
    Unaddressed,
    /// An action this World does not interpret. Reported verbatim so the app
    /// that owns the kind can act on it.
    Kind {
        kind: String,
        settings: Settings,
    },
}

/// Which rung answered, by name.
///
/// The name is carried because "why is this screen showing that" is the
/// question asked when it is showing the wrong thing, and a bare id cannot
/// answer it in a sentence somebody can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum Resolved {
    Broadcast {
        broadcast: String,
        name: String,
        audience: String,
        priority: i16,
    },
    Channel {
        channel: String,
        name: String,
        /// The window inside the channel, when one was open.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
    },
}

/// What a screen plays, why, and when that changes on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playback {
    pub showing: Showing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Resolved>,
    /// The earliest moment this answer changes with nobody writing, so a
    /// receiver can sleep until then instead of polling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_boundary_unix_ms: Option<u64>,
}

/// Everything addressed at one screen, resolved.
///
/// Pure over its arguments and total: an unparseable window degrades that
/// window rather than failing the screen, because one malformed broadcast must
/// not be able to darken a fleet.
pub fn resolve(
    screen: &SignageScreen,
    channels: &[SignageChannel],
    broadcasts: &[SignageBroadcast],
    cx: &Context,
    lookup: &impl AudienceLookup,
) -> Playback {
    let mut boundary = Boundary::default();

    let superseded: std::collections::BTreeSet<&str> = broadcasts
        .iter()
        .filter(|candidate| !candidate.cancelled_by(cx.now_unix_ms))
        .flat_map(|candidate| candidate.supersedes.iter().map(String::as_str))
        .collect();

    let mut winner: Option<&SignageBroadcast> = None;
    for broadcast in broadcasts {
        if broadcast.cancelled_by(cx.now_unix_ms) || superseded.contains(broadcast.id.as_str()) {
            continue;
        }
        let open = match &broadcast.timing {
            Timing::Window { window } => match window.evaluate_at(cx.now_unix_ms) {
                Ok((active, next)) => {
                    boundary.saw(next);
                    active
                }
                // A window nobody can evaluate addresses nobody, and says so
                // by omission rather than by taking the fleet down with it.
                Err(_) => false,
            },
            Timing::When { of, .. } => of.reaches(screen, cx, lookup),
        };
        if !open {
            continue;
        }
        let Some(rule) = lookup.audience(&broadcast.audience) else {
            continue;
        };
        if !rule.reaches(screen, cx, lookup) {
            continue;
        }
        let better = winner.is_none_or(|current| {
            let (mine, theirs) = (broadcast.timing.priority(), current.timing.priority());
            mine > theirs || (mine == theirs && broadcast.id < current.id)
        });
        if better {
            winner = Some(broadcast);
        }
    }

    if let Some(broadcast) = winner {
        let source = Some(Resolved::Broadcast {
            broadcast: broadcast.id.clone(),
            name: broadcast.name.clone(),
            audience: broadcast.audience.clone(),
            priority: broadcast.timing.priority(),
        });
        match &broadcast.action {
            Action::Play { program } => {
                return Playback {
                    showing: Showing::Program {
                        program: program.clone(),
                    },
                    source,
                    next_boundary_unix_ms: boundary.soonest(),
                }
            }
            Action::Blank => {
                return Playback {
                    showing: Showing::Blank,
                    source,
                    next_boundary_unix_ms: boundary.soonest(),
                }
            }
            Action::Kind { kind, settings } => {
                return Playback {
                    showing: Showing::Kind {
                        kind: kind.clone(),
                        settings: settings.clone(),
                    },
                    source,
                    next_boundary_unix_ms: boundary.soonest(),
                }
            }
            // Both fall through to a channel: `Tune` to the one it names,
            // `Restore` to the one the screen is already on. Their effect is
            // to outrank everything below them, which is what an all-clear is.
            Action::Tune { channel } => return from_channel(channel, channels, &mut boundary, cx),
            Action::Restore => {}
        }
    }

    match screen.tuned.as_deref() {
        Some(channel) => from_channel(channel, channels, &mut boundary, cx),
        None => Playback {
            showing: Showing::Unaddressed,
            source: None,
            next_boundary_unix_ms: boundary.soonest(),
        },
    }
}

fn from_channel(
    id: &str,
    channels: &[SignageChannel],
    boundary: &mut Boundary,
    cx: &Context,
) -> Playback {
    let Some(channel) = channels.iter().find(|candidate| candidate.id == id) else {
        // Tuned to something that is not here. Unaddressed, and honestly so —
        // a screen pointed at a deleted channel is not the same as a screen
        // pointed at nothing, but it shows the same thing and the operator
        // finds out from the absent source rather than an invented one.
        return Playback {
            showing: Showing::Unaddressed,
            source: None,
            next_boundary_unix_ms: boundary.soonest(),
        };
    };

    let mut open: Option<&ChannelWindow> = None;
    for window in &channel.schedule {
        let Ok((active, next)) = window.window.evaluate_at(cx.now_unix_ms) else {
            continue;
        };
        boundary.saw(next);
        if active
            && open.is_none_or(|current| {
                window.window.priority > current.window.priority
                    || (window.window.priority == current.window.priority && window.id < current.id)
            })
        {
            open = Some(window);
        }
    }

    let (program, window_id) = match open {
        Some(window) => (Some(window.program.clone()), Some(window.id.clone())),
        None => (channel.base.clone(), None),
    };

    Playback {
        showing: match program {
            Some(program) => Showing::Program { program },
            None => Showing::Unaddressed,
        },
        source: Some(Resolved::Channel {
            channel: channel.id.clone(),
            name: channel.name.clone(),
            window: window_id,
        }),
        next_boundary_unix_ms: boundary.soonest(),
    }
}
