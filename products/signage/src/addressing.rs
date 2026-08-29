//! Who a transmission reaches, and what is true of the screen it reaches.
//!
//! Addressing is a *predicate*, never a membership list. The distinction is the
//! whole point: a list is correct at the moment it is written and rots as the
//! fleet grows, which is exactly when addressing starts to matter. Xibo shipped
//! the materialised version and its "dynamic" groups recomputed only when
//! somebody re-saved them, which is a static group wearing a costume.
//!
//! The shape is [SCTE-224]'s: a named, reusable `Audience` referenced by an
//! action, evaluated per viewer at resolution time. What that standard matches
//! on is subscribers; what this matches on is screens, their facts, and what
//! they have most recently observed — which is the seam that keeps this from
//! being cable television with the words changed.
//!
//! [SCTE-224]: ANSI/SCTE 224, Event Scheduling and Notification Interface

use replica::body::{BodyId, BodyKey};
use serde::{Deserialize, Serialize};

use crate::contract::{
    body_key, valid_kind, MAX_AUDIENCE_HOPS, MAX_LABEL_CHARS, MAX_MATCH_DEPTH, MAX_MATCH_TERMS,
    MAX_NAME_CHARS, MAX_SETTING_CHARS,
};
use crate::fleet::SignageScreen;

/// A panel's geography.
///
/// Typed here, and only this much, because every location-aware kind wants it
/// and none of it is anyone's jurisprudence: an athan card computes prayer
/// times from a coordinate, a weather card a forecast, a clock an offset. What
/// a kind *does* with a place is the kind's business and stays in
/// [`SignageScreen::facts`], untyped, for the same reason `Settings` is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub latitude: f64,
    pub longitude: f64,
    /// IANA identifier. Required, because a coordinate without one computes a
    /// plausible timetable for the wrong offset and nothing looks wrong.
    pub timezone: String,
    /// A coarse administrative name an audience can match on without anybody
    /// maintaining a parallel label that drifts from the coordinate beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

impl Place {
    pub fn validate(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && !self.timezone.trim().is_empty()
            && self.timezone.chars().count() <= MAX_NAME_CHARS
            && self
                .region
                .as_ref()
                .is_none_or(|region| region.chars().count() <= MAX_NAME_CHARS)
    }

    /// Great-circle distance in kilometres, against a bound the author wrote.
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::float_arithmetic,
        clippy::suboptimal_flops,
        reason = "bounded trigonometry over validated finite coordinates"
    )]
    fn km_to(&self, latitude: f64, longitude: f64) -> f64 {
        const EARTH_KM: f64 = 6371.0;
        let (lat1, lat2) = (self.latitude.to_radians(), latitude.to_radians());
        let dlat = lat2 - lat1;
        let dlon = (longitude - self.longitude).to_radians();
        let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        2.0 * EARTH_KM * a.sqrt().clamp(0.0, 1.0).asin()
    }
}

/// What an audience can ask about a place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlaceMatch {
    /// Sited at all. `Not(Placed)` is every screen nobody has located yet,
    /// which is a fleet an installer wants to be able to ask for.
    Placed,
    Region {
        region: String,
    },
    Timezone {
        timezone: String,
    },
    Within {
        latitude: f64,
        longitude: f64,
        km: f64,
    },
}

/// What a screen last reported about itself, in its own vocabulary.
///
/// Stored as strings for the same reason settings are: this World does not
/// know what any kind senses, and typing it here would put every sensor's
/// schema in the substrate.
pub type Observations = std::collections::BTreeMap<String, String>;

/// What a match is evaluated against.
///
/// The impurity lives here, at the boundary, assembled by the caller. The
/// evaluator is a total function of this value — which is what lets two
/// replicas agree while still admitting a sensor. Time is one observation
/// among others rather than the only one the model can see.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    pub now_unix_ms: u64,
    /// Empty is not zero. An absent observation fails every comparison rather
    /// than reading as a false one.
    pub observations: Observations,
}

impl Context {
    pub fn at(now_unix_ms: u64) -> Self {
        Self {
            now_unix_ms,
            observations: Observations::new(),
        }
    }

    pub fn observing(now_unix_ms: u64, observations: Observations) -> Self {
        Self {
            now_unix_ms,
            observations,
        }
    }
}

/// How an observation is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    Is,
    IsNot,
    /// Numeric. A value that does not parse fails, and never reads as zero.
    Above,
    Below,
}

impl Compare {
    fn is() -> Self {
        Self::Is
    }

    fn holds(self, observed: &str, against: &str) -> bool {
        match self {
            Self::Is => observed == against,
            Self::IsNot => observed != against,
            Self::Above | Self::Below => {
                let (Ok(left), Ok(right)) = (observed.parse::<f64>(), against.parse::<f64>())
                else {
                    return false;
                };
                if matches!(self, Self::Above) {
                    left > right
                } else {
                    left < right
                }
            }
        }
    }
}

/// Who a transmission reaches.
///
/// Tagged `match` rather than `kind`, because `Fact` carries a field by that
/// name and a kind is a thing this enum talks *about*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "match", rename_all = "snake_case")]
pub enum Match {
    /// Every screen. The emergency case, and the one that has to be explicit
    /// rather than reachable by leaving a field empty.
    All,
    /// Exactly one. The single-screen rental.
    Screen {
        screen: String,
    },
    Label {
        label: String,
    },
    Place {
        place: PlaceMatch,
    },
    /// A fact a kind stored about this venue.
    Fact {
        kind: String,
        key: String,
        value: String,
    },
    /// Everything tuned to a channel.
    Tuned {
        channel: String,
    },
    /// Something the screen reported. The seam that makes an audience
    /// reactive rather than merely scheduled.
    Observed {
        key: String,
        #[serde(default = "Compare::is")]
        compare: Compare,
        value: String,
    },
    /// Another named audience, by reference. Bounded, and acyclic by write.
    Audience {
        audience: String,
    },
    Not {
        of: Box<Match>,
    },
    AllOf {
        of: Vec<Match>,
    },
    AnyOf {
        of: Vec<Match>,
    },
}

/// Resolves [`Match::Audience`] during evaluation. Supplied by the caller so
/// the evaluator stays a pure function of what it is handed.
pub trait AudienceLookup {
    fn audience(&self, id: &str) -> Option<&Match>;
}

/// No references resolvable — every `Audience` term reaches nobody.
impl AudienceLookup for () {
    fn audience(&self, _id: &str) -> Option<&Match> {
        None
    }
}

impl AudienceLookup for std::collections::BTreeMap<String, Match> {
    fn audience(&self, id: &str) -> Option<&Match> {
        self.get(id)
    }
}

impl Match {
    /// Structural bounds, checked at write so evaluation never has to.
    ///
    /// A term naming something unparseable is refused here rather than
    /// silently never matching — an audience that quietly addresses nobody is
    /// the worst failure this object has, because it is indistinguishable from
    /// one that addresses nobody on purpose.
    pub fn validate(&self) -> bool {
        self.validate_to(MAX_MATCH_DEPTH)
    }

    fn validate_to(&self, depth: u8) -> bool {
        let Some(deeper) = depth.checked_sub(1) else {
            return false;
        };
        match self {
            Self::All => true,
            Self::Screen { screen } => BodyId::parse(screen).is_some(),
            Self::Tuned { channel } => BodyId::parse(channel).is_some(),
            Self::Audience { audience } => BodyId::parse(audience).is_some(),
            Self::Label { label } => valid_label(label),
            Self::Place { place } => match place {
                PlaceMatch::Placed => true,
                PlaceMatch::Region { region } => !region.trim().is_empty(),
                PlaceMatch::Timezone { timezone } => !timezone.trim().is_empty(),
                PlaceMatch::Within {
                    latitude,
                    longitude,
                    km,
                } => {
                    (-90.0..=90.0).contains(latitude)
                        && (-180.0..=180.0).contains(longitude)
                        && km.is_finite()
                        && *km > 0.0
                }
            },
            Self::Fact { kind, key, value } => {
                valid_kind(kind)
                    && !key.is_empty()
                    && key.chars().count() <= MAX_NAME_CHARS
                    && value.chars().count() <= MAX_SETTING_CHARS
            }
            Self::Observed { key, value, .. } => {
                !key.is_empty()
                    && key.chars().count() <= MAX_NAME_CHARS
                    && value.chars().count() <= MAX_SETTING_CHARS
            }
            Self::Not { of } => of.validate_to(deeper),
            Self::AllOf { of } | Self::AnyOf { of } => {
                !of.is_empty()
                    && of.len() <= MAX_MATCH_TERMS
                    && of.iter().all(|term| term.validate_to(deeper))
            }
        }
    }

    /// Does this reach that screen, right now?
    ///
    /// Total, pure over `(screen, cx, lookup)`, and terminating by
    /// construction: nesting is bounded at write and reference-following by
    /// `hops`. Two replicas handed the same three arguments answer the same
    /// way, which is what the whole addressing model rests on.
    pub fn reaches(
        &self,
        screen: &SignageScreen,
        cx: &Context,
        lookup: &impl AudienceLookup,
    ) -> bool {
        self.reaches_within(screen, cx, lookup, MAX_AUDIENCE_HOPS)
    }

    fn reaches_within(
        &self,
        screen: &SignageScreen,
        cx: &Context,
        lookup: &impl AudienceLookup,
        hops: u8,
    ) -> bool {
        match self {
            Self::All => true,
            Self::Screen { screen: id } => &screen.id == id,
            Self::Label { label } => screen.labels.iter().any(|held| held == label),
            Self::Tuned { channel } => screen.tuned.as_deref() == Some(channel.as_str()),
            Self::Place { place } => match (&screen.place, place) {
                (None, _) => false,
                (Some(_), PlaceMatch::Placed) => true,
                (Some(sited), PlaceMatch::Region { region }) => sited
                    .region
                    .as_ref()
                    .is_some_and(|held| held.eq_ignore_ascii_case(region)),
                (Some(sited), PlaceMatch::Timezone { timezone }) => sited.timezone == *timezone,
                (
                    Some(sited),
                    PlaceMatch::Within {
                        latitude,
                        longitude,
                        km,
                    },
                ) => sited.km_to(*latitude, *longitude) <= *km,
            },
            Self::Fact { kind, key, value } => screen
                .facts
                .get(kind)
                .and_then(|settings| settings.get(key))
                .is_some_and(|held| held == value),
            Self::Observed {
                key,
                compare,
                value,
            } => cx
                .observations
                .get(key)
                .is_some_and(|observed| compare.holds(observed, value)),
            Self::Audience { audience } => match hops.checked_sub(1) {
                None => false,
                Some(fewer) => lookup
                    .audience(audience)
                    .is_some_and(|rule| rule.reaches_within(screen, cx, lookup, fewer)),
            },
            Self::Not { of } => !of.reaches_within(screen, cx, lookup, hops),
            Self::AllOf { of } => of
                .iter()
                .all(|term| term.reaches_within(screen, cx, lookup, hops)),
            Self::AnyOf { of } => of
                .iter()
                .any(|term| term.reaches_within(screen, cx, lookup, hops)),
        }
    }

    /// Every audience this rule names, for the cycle check at write.
    pub fn referenced_audiences(&self, into: &mut Vec<String>) {
        match self {
            Self::Audience { audience } => into.push(audience.clone()),
            Self::Not { of } => of.referenced_audiences(into),
            Self::AllOf { of } | Self::AnyOf { of } => {
                for term in of {
                    term.referenced_audiences(into);
                }
            }
            _ => {}
        }
    }
}

/// A named, reusable audience — what the interface presents as a saved view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignageAudience {
    pub id: String,
    pub name: String,
    pub rule: Match,
}

impl SignageAudience {
    pub fn validate(&self) -> bool {
        BodyId::parse(&self.id).is_some()
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && self.rule.validate()
    }

    pub fn body_key(&self) -> Option<BodyKey> {
        body_key(&self.id)
    }
}

/// Labels are the operator's own vocabulary, so the grammar only forbids what
/// would make one ambiguous to write down: whitespace and the separator a
/// list of them would use.
pub fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.chars().count() <= MAX_LABEL_CHARS
        && label
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b',' && b != b' ')
}
