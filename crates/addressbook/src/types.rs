//! Durable nouns. Derived observations are typed here and never serialized
//! into the Book.

use serde::{Deserialize, Serialize};

use mechanics::ids::{ActorId, DeviceId, SpaceId};

use crate::ids::{CardId, PathHash};

/// Who is writing, and the wall clock they offer for display-only `Stamp.at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// The device minting this mutation's stamps and tags.
    pub device: DeviceId,
    /// Unix milliseconds. Display only; never a tiebreak.
    pub at: u64,
}

/// Causal stamp on an authored fact. Order is `(lamport, by)`; `at` is display-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub lamport: u64,
    pub by: DeviceId,
    pub at: u64,
}

impl PartialOrd for Stamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Stamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.lamport
            .cmp(&other.lamport)
            .then_with(|| self.by.cmp(&other.by))
    }
}

/// A last-writer field projected from per-device candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field<T> {
    pub value: T,
    pub stamp: Stamp,
}

/// How a handle or group link was asserted. Never `Derived`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Evidence {
    Declared,
    Asserted { from: CardId },
}

/// Unique per add so an unlink then a re-link is a new set member.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Tag {
    pub device: DeviceId,
    pub counter: u64,
}

/// A stable identity a Card can name. `LocalAgent` never leaves this machine.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Handle {
    Device(DeviceId),
    Actor { space: SpaceId, actor: ActorId },
    LocalAgent { store: PathHash, name: String },
}

impl Handle {
    /// Whether this handle is permitted on an artifact that leaves the device.
    pub fn may_leave_device(&self) -> bool {
        !matches!(self, Self::LocalAgent { .. })
    }

    /// Parse a wire spelling: HandleKey hex, a device id, `actor:<ws>:<act>`,
    /// or `agent:<pathhash>:<name>`.
    pub fn parse_wire(raw: &str) -> Result<Self, crate::Error> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(crate::Error::Invalid("empty handle"));
        }
        if let Ok(handle) = HandleKey::from_encoded(raw.to_owned()).decode() {
            return Ok(handle);
        }
        if let Some(id) = DeviceId::parse(raw) {
            return Ok(Self::Device(id));
        }
        if let Some(rest) = raw.strip_prefix("actor:") {
            let (space, actor) = rest
                .split_once(':')
                .ok_or(crate::Error::Invalid("actor handle"))?;
            let space = SpaceId::parse(space).ok_or(crate::Error::Invalid("actor space"))?;
            let actor = ActorId::parse(actor).ok_or(crate::Error::Invalid("actor id"))?;
            return Ok(Self::Actor { space, actor });
        }
        if let Some(rest) = raw.strip_prefix("agent:") {
            let (store, name) = rest
                .split_once(':')
                .ok_or(crate::Error::Invalid("agent handle"))?;
            let store = PathHash::parse(store).ok_or(crate::Error::Invalid("agent store"))?;
            if name.is_empty() {
                return Err(crate::Error::Invalid("agent name"));
            }
            return Ok(Self::LocalAgent {
                store,
                name: name.to_owned(),
            });
        }
        Err(crate::Error::Invalid("unrecognized handle"))
    }

    /// Stable wire spelling for a handle that may leave this device, or a
    /// local-agent form that must not.
    pub fn to_wire(&self) -> String {
        match self {
            Self::Device(id) => id.as_str().to_owned(),
            Self::Actor { space, actor } => format!("actor:{}:{}", space.as_str(), actor.as_str()),
            Self::LocalAgent { store, name } => format!("agent:{}:{name}", store.as_str()),
        }
    }
}

/// Version-tagged canonical encoding of a [`Handle`]. Persistence surface.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HandleKey(String);

impl HandleKey {
    /// Encode `handle` as the frozen v1 key.
    pub fn encode(handle: &Handle) -> Result<Self, crate::Error> {
        crate::codec::handle_key(handle)
    }

    /// Decode a stored key.
    pub fn decode(&self) -> Result<Handle, crate::Error> {
        crate::codec::handle_from_key(self)
    }

    /// The encoded bytes as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_encoded(raw: String) -> Self {
        Self(raw)
    }
}

/// One handle link on a Card.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Link {
    pub handle: Handle,
    pub tag: Tag,
    pub evidence: Evidence,
    pub added: Stamp,
    pub last_seen: Option<Stamp>,
}

/// One group membership link.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GroupLink {
    pub name: String,
    pub tag: Tag,
    pub added: Stamp,
}

/// A projected Card. Authored fields only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: CardId,
    pub name: Field<String>,
    pub note: Field<String>,
    /// The card's picture, stored as `<mime>;base64,<data>` — or empty when
    /// none was authored, in which case every client draws its default face.
    /// An additive path: a book written before pictures existed projects an
    /// empty one, and an older reader skips the path it does not know.
    pub picture: Field<String>,
    pub groups: Vec<GroupLink>,
    pub handles: Vec<Link>,
    pub self_claim: Option<Stamp>,
    pub created: Stamp,
}

/// Live projection of the Book. No derived fact is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub version: u8,
    pub cards: std::collections::BTreeMap<CardId, Card>,
    pub graves: std::collections::BTreeMap<CardId, Stamp>,
    pub redirects: std::collections::BTreeMap<CardId, (CardId, Stamp)>,
    pub clock: u64,
    /// Per-device tag counters. Persist so an unlink then a re-link cannot
    /// reuse a tag Fabric already observed.
    pub tag_counters: std::collections::BTreeMap<DeviceId, u64>,
}

/// A handle as seen from a Card, for the daemon to adapt into a `HandleView`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleView {
    pub handle: Handle,
    pub card: CardId,
    pub tag: Tag,
    pub evidence: Evidence,
}

impl Book {
    /// Authored lookup: Cards that carry this handle, following redirects.
    pub fn authored_cards_for(&self, handle: &Handle) -> Vec<CardId> {
        let mut hits = Vec::new();
        for (id, card) in &self.cards {
            if card.handles.iter().any(|link| &link.handle == handle) {
                hits.push(id.clone());
            }
        }
        hits
    }

    /// Every handle currently hanging off live Cards.
    pub fn handle_views(&self) -> Vec<HandleView> {
        let mut views = Vec::new();
        for (id, card) in &self.cards {
            for link in &card.handles {
                views.push(HandleView {
                    handle: link.handle.clone(),
                    card: id.clone(),
                    tag: link.tag.clone(),
                    evidence: link.evidence.clone(),
                });
            }
        }
        views
    }
}

/// Why a derived observation is not a fact. Never written to the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Partial,
    Stale,
    Unavailable,
}

/// A derived observation about a handle. Constructed by the daemon over an
/// active snapshot; the leaf crate never persists one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedObservation {
    pub handle: Handle,
    pub coverage: Coverage,
}

/// What a scoped resolution may return. Authored hits plus coverage of the
/// snapshot they were decorated against. The derived half is never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub authored: Vec<HandleView>,
    pub coverage: Option<Coverage>,
}
