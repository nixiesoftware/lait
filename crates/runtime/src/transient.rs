//! Transient collaboration state: cursors, presence, typing, residency hints.
//!
//! Everything here is state a Station currently believes and will happily
//! forget. It is never journaled, never a Body, never an Observation, and never
//! survives a restart — a caret that outlived the tab holding it is a ghost, and
//! a presence that survived a crash is a lie about who is here.
//!
//! That is the whole design constraint, and it produces three rules the rest of
//! the module is built from.
//!
//! **Nothing has a goodbye.** A tab closes, a laptop sleeps, a network drops —
//! none of those deliver a message. So every slot carries its own expiry and
//! disappears without anyone saying so. Retirement is an optimisation on top of
//! that, never a prerequisite.
//!
//! **Epochs are compared, never ordered.** A `connection_epoch` is 16 random
//! bytes minted per reconnect. Two of them have no order, so "is this stale"
//! can only be equality against the epoch this session was admitted at.
//! Anything that looked like a comparison would be reading noise as sequence.
//!
//! **The table is bounded and every entry is evictable.** Nothing in it is
//! correctness, so eviction costs a stale cursor. An unbounded table would be a
//! Station that a Space can make allocate without ever committing anything.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::budget::{deadline, slots};

/// The largest encoded transient item this build will decode.
///
/// A caret is a position and an anchor; a selection is two. Nothing here is
/// user text — the payload names *where* something is, never *what* it says —
/// so this is generous for what it carries and tight against a peer that would
/// like it not to be.
pub const MAX_TRANSIENT_ITEM_BYTES: usize = 4 * 1024;

/// The longest field path a scope may name.
///
/// A field path is a schema-declared name, so it is short by construction. It
/// is bounded separately from the item because it reaches further: it becomes
/// part of a container key inside the collaborative document.
pub const MAX_SCOPE_FIELD_BYTES: usize = 128;

/// The longest encoded anchor a payload may carry.
///
/// `Anchor::decode_canonical` bounds its head set and enforces re-encode
/// equality, and places no bound at all on `path` — while `MemoryEngine::resolve`
/// feeds that path into loro's container namespace twice, once through
/// `doc.get_text(typed_key("text", &path))` and once through a `ContainerID::Root`
/// name. A mismatched Body is already safe there; the path is not. This is the
/// bound that makes it so, applied before anything resolves.
pub const MAX_ANCHOR_BYTES: usize = 2 * 1024;

/// What a transient item is about.
///
/// Every Body-naming variant carries `world` as well as `body`, because a Body
/// is addressed by both — `BodyKey { world, body }` — and an anchor that named
/// only the body would resolve against whichever World asked. Operation ids
/// collide across documents of one activation, so that is not a lookup miss, it
/// is a plausible and silently wrong answer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Target {
    /// Somebody is looking at this issue.
    Body { world: String, body: [u8; 16] },
    /// Somebody is looking at this document.
    Material { world: String, body: [u8; 16] },
    /// Somebody's cursor is in this field of this Body.
    Field {
        world: String,
        body: [u8; 16],
        field: String,
    },
    /// Somebody is typing in this field.
    Typing {
        world: String,
        body: [u8; 16],
        field: String,
    },
    /// How much of this content a peer holds. A hint, never a promise —
    /// residency is answered by asking, and this only says who to ask first.
    Content { content: [u8; 32] },
    /// A World's own scope. Opaque here: the substrate carries it and does not
    /// interpret it.
    World {
        world: String,
        schema: String,
        key: String,
    },
}

impl Target {
    /// The field this scope names, when it names one.
    pub fn field(&self) -> Option<&str> {
        match self {
            Self::Field { field, .. } | Self::Typing { field, .. } => Some(field),
            _ => None,
        }
    }

    /// The same bounds, for a caller outside this module.
    ///
    /// A signal carries a scope and has to check it before anything acts on it,
    /// and duplicating the rule would be two rules.
    pub fn validate_wire(&self) -> Result<(), Invalid> {
        self.validate()
    }

    fn validate(&self) -> Result<(), Invalid> {
        // A field path is a name inside a Body's collaborative schema, so it has
        // no grammar of its own here — only a bound, which is load-bearing
        // because the path reaches loro's container namespace.
        let bounded = |value: &str| {
            if value.len() > MAX_SCOPE_FIELD_BYTES || value.is_empty() {
                Err(Invalid::Bounds)
            } else {
                Ok(())
            }
        };
        // Parsed through the real grammar rather than measured, which is what
        // `Signal::WorldSignal` already does and what this used to skip. Its
        // comment states the rule both shapes now follow: a World id has a
        // shape, and something that is merely short is not therefore one. The
        // two World-facing shapes disagreeing about what a World id *is* is how
        // a scope and a signal about the same World stop matching.
        let world_id = |value: &str| replica::ids::WorldId::parse(value).ok_or(Invalid::Malformed);
        match self {
            Self::Body { world, .. } | Self::Material { world, .. } => {
                world_id(world)?;
                Ok(())
            }
            Self::Field { world, field, .. } | Self::Typing { world, field, .. } => {
                world_id(world)?;
                bounded(field)
            }
            Self::Content { .. } => Ok(()),
            Self::World { world, schema, key } => {
                world_id(world)?;
                replica::ids::SchemaId::parse(schema).ok_or(Invalid::Malformed)?;
                // The key is the World's own, so it gets a bound and no grammar
                // — the substrate has no opinion about what a World calls its
                // rows. What the World itself declared is enforced above this,
                // where the registry is in scope.
                bounded(key)
            }
        }
    }
}

/// What is being said about a scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransientPayload {
    /// Somebody is here. Carries nothing but the fact.
    Presence,
    /// A cursor position.
    Caret { anchor: Vec<u8> },
    /// A cursor with a range behind it.
    Selection { anchor: Vec<u8>, focus: Vec<u8> },
    /// Somebody is typing. Coarse by design — "typing" has no intermediate
    /// values worth sending.
    Typing,
    /// Which chunks of a content a peer holds.
    Residency { chunks: Vec<u32> },
}

/// The kind of a payload, for the legality table.
///
/// Derived from the payload and never sent — a kind on the wire would be a
/// second source of truth about what a payload is, and the two would eventually
/// disagree in a way only an attacker would notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransientKind {
    Presence,
    Caret,
    Selection,
    Typing,
    Residency,
}

impl TransientPayload {
    pub fn kind(&self) -> TransientKind {
        match self {
            Self::Presence => TransientKind::Presence,
            Self::Caret { .. } => TransientKind::Caret,
            Self::Selection { .. } => TransientKind::Selection,
            Self::Typing => TransientKind::Typing,
            Self::Residency { .. } => TransientKind::Residency,
        }
    }

    fn validate(&self, scope: &Target) -> Result<(), Invalid> {
        // The legality table. It is what makes `MAX_SLOTS_PER_CONNECTION =
        // MAX_SUBSCRIBED_SCOPES_PER_CONNECTION * 2` a fact rather than a hope:
        // no scope admits more than two kinds.
        // Written as a table rather than a `matches!` because the pairs are
        // the specification: one line per legal combination, so adding a scope
        // without deciding what it may carry is visible here rather than
        // implied by an absence.
        #[allow(clippy::match_like_matches_macro)]
        let legal = match (scope, self.kind()) {
            (Target::Body { .. }, TransientKind::Presence) => true,
            (Target::Material { .. }, TransientKind::Presence) => true,
            (Target::Field { .. }, TransientKind::Caret) => true,
            (Target::Field { .. }, TransientKind::Selection) => true,
            (Target::Typing { .. }, TransientKind::Typing) => true,
            (Target::Content { .. }, TransientKind::Residency) => true,
            (Target::World { .. }, TransientKind::Presence) => true,
            _ => false,
        };
        if !legal {
            return Err(Invalid::IllegalForScope);
        }
        for anchor in self.anchors() {
            if anchor.len() > MAX_ANCHOR_BYTES {
                return Err(Invalid::Bounds);
            }
            // The path inside the anchor has to be the field the scope names.
            //
            // Not a consistency nicety: the path becomes a loro container key,
            // and an anchor free to name any path is a peer choosing which
            // container a resolve touches. Binding it to the subscribed scope
            // means a peer can only ask about what it already said it was
            // watching.
            let decoded =
                replica::Anchor::decode_canonical(anchor).map_err(|_| Invalid::Malformed)?;
            if decoded.path.len() > MAX_SCOPE_FIELD_BYTES {
                return Err(Invalid::Bounds);
            }
            match scope.field() {
                Some(field) if decoded.path == field => {}
                _ => return Err(Invalid::AnchorOutsideScope),
            }
        }
        if let Self::Residency { chunks } = self {
            if chunks.len() > MAX_RESIDENCY_CHUNKS {
                return Err(Invalid::Bounds);
            }
        }
        Ok(())
    }

    fn anchors(&self) -> Vec<&Vec<u8>> {
        match self {
            Self::Caret { anchor } => vec![anchor],
            Self::Selection { anchor, focus } => vec![anchor, focus],
            _ => Vec::new(),
        }
    }
}

/// How many chunk indices one residency hint may name. A hint is a suggestion
/// about who to ask, so a complete bitmap is not what it is for.
pub const MAX_RESIDENCY_CHUNKS: usize = 256;

/// One thing a peer said, as it arrives.
///
/// No station and no timestamp. The station is whoever the connection was
/// admitted as — carrying it in the item would let a peer speak for another —
/// and the timestamp is when *we* saw it, because a peer's clock is a peer's
/// claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransientItem {
    pub connection_epoch: [u8; 16],
    pub seq: u64,
    pub scope: Target,
    pub payload: TransientPayload,
}

impl TransientItem {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard transient item")
    }

    /// Decode one item, in the order that makes each check protect the next.
    ///
    /// Textually the order `FreightFrame::decode_canonical` uses, and for the
    /// same reasons: the outer ceiling first so no allocation is sized by a
    /// peer, then postcard, then re-encode equality so one item has one
    /// spelling, then the semantic checks.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_TRANSIENT_ITEM_BYTES {
            return Err(Invalid::TooLarge);
        }
        let item: Self = postcard::from_bytes(bytes).map_err(|_| Invalid::Malformed)?;
        if item.encode() != bytes {
            return Err(Invalid::NonCanonical);
        }
        item.validate()?;
        Ok(item)
    }

    pub fn validate(&self) -> Result<(), Invalid> {
        self.scope.validate()?;
        self.payload.validate(&self.scope)
    }

    /// The table key. A scope plus a kind, because one peer may hold a caret
    /// and a selection in the same field without either replacing the other.
    fn slot_key(&self) -> (Target, u8) {
        (self.scope.clone(), self.payload.kind() as u8)
    }
}

/// Why an item was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    TooLarge,
    Malformed,
    NonCanonical,
    Bounds,
    /// This payload is not a thing that scope can carry.
    IllegalForScope,
    /// The anchor names a path the subscribed scope does not.
    AnchorOutsideScope,
    /// A World's own scope naming a schema that World never declared.
    ///
    /// Distinct from `Malformed`, which is about shape. This one parsed and is
    /// simply not a thing the World in question said it has — the same answer a
    /// `WorldSignal` for an undeclared schema gets, and for the same reason:
    /// acting on it would be acting on a schema nobody reviewed.
    NotDeclared,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// One live entry.
#[derive(Debug, Clone, PartialEq)]
pub struct TransientSlot {
    pub connection_epoch: [u8; 16],
    pub seq: u64,
    /// When this Station saw it. Ours, not theirs.
    pub arrived_at: Instant,
    /// Set when the peer retired the scope. The slot lingers briefly rather
    /// than vanishing, so a datagram already in flight cannot resurrect it.
    pub retired_at: Option<Instant>,
    pub payload: TransientPayload,
}

/// What happened to an admitted item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// Stored, replacing whatever that slot held.
    Stored,
    /// Superseded by something already stored — an out-of-order datagram.
    Stale,
    /// From a session epoch this connection is not admitted at.
    ///
    /// Equality, never a comparison: two random epochs have no order, so the
    /// only answerable question is whether this is the one we admitted.
    WrongEpoch,
    /// At or below a retirement watermark. The peer said it was done with this
    /// scope, and this datagram was already in flight when it did.
    Retired,
    /// The item is fine and there was no room. Costs a stale cursor.
    Evicted,
    Refused(Invalid),
}

/// The bounded table of what peers currently believe.
pub struct TransientStore {
    slots: BTreeMap<(Target, u8), TransientSlot>,
    /// Per slot key, the highest `(epoch, seq)` a retirement covered.
    ///
    /// Kept after the slot is gone, for a grace window, because retirement and
    /// a datagram already on the wire race by nature — and losing that race
    /// resurrects a cursor for a full TTL.
    retired: BTreeMap<(Target, u8), ([u8; 16], u64, Instant)>,
    capacity: usize,
}

impl TransientStore {
    pub fn new() -> Self {
        Self::with_capacity(slots::MAX_TRANSIENT_SLOTS)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            retired: BTreeMap::new(),
            capacity: capacity.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Offer one item, from a session admitted at `admitted_epoch`.
    pub fn admit(
        &mut self,
        item: &TransientItem,
        admitted_epoch: &[u8; 16],
        now: Instant,
    ) -> AdmitOutcome {
        if let Err(error) = item.validate() {
            return AdmitOutcome::Refused(error);
        }
        if &item.connection_epoch != admitted_epoch {
            return AdmitOutcome::WrongEpoch;
        }
        let key = item.slot_key();
        if let Some((epoch, seq, _)) = self.retired.get(&key) {
            if epoch == &item.connection_epoch && item.seq <= *seq {
                return AdmitOutcome::Retired;
            }
        }
        if let Some(existing) = self.slots.get(&key) {
            // Within one epoch a sequence is a sequence. Across epochs it is
            // not, and the epoch check above has already established they are
            // the same one.
            if existing.seq >= item.seq {
                return AdmitOutcome::Stale;
            }
        } else if self.slots.len() >= self.capacity {
            return AdmitOutcome::Evicted;
        }
        self.slots.insert(
            key,
            TransientSlot {
                connection_epoch: item.connection_epoch,
                seq: item.seq,
                arrived_at: now,
                retired_at: None,
                payload: item.payload.clone(),
            },
        );
        AdmitOutcome::Stored
    }

    /// Record that a peer is finished with a scope.
    ///
    /// The watermark outlives the slot: a datagram sent before the retirement
    /// arrived is still in flight, and admitting it would rebuild the slot for
    /// a full TTL after the peer said it was done.
    pub fn retire(
        &mut self,
        scope: &Target,
        kind: TransientKind,
        connection_epoch: [u8; 16],
        seq: u64,
        now: Instant,
    ) {
        let key = (scope.clone(), kind as u8);
        // Only what the retirement actually covers. The watermark it records is
        // `seq`, and `admit` refuses items at or below it — so removing a slot
        // holding a *higher* sequence would delete something this retirement
        // does not cover and which the watermark would then let straight back
        // in. That is the same race the watermark exists for, seen from the
        // other side: retirement and a datagram in flight are unordered, and
        // whichever arrives second must not undo the newer of the two.
        let covered = self
            .slots
            .get(&key)
            .is_none_or(|slot| slot.connection_epoch != connection_epoch || slot.seq <= seq);
        if covered {
            self.slots.remove(&key);
        }
        self.retired.insert(key, (connection_epoch, seq, now));
    }

    /// Drop what has expired. Called on a beat; nothing depends on when.
    pub fn sweep(&mut self, now: Instant) -> usize {
        let before = self.slots.len();
        self.slots.retain(|key, slot| {
            let ttl = match key.0 {
                Target::Body { .. } | Target::Material { .. } | Target::World { .. } => {
                    deadline::PRESENCE_TTL
                }
                _ => deadline::CURSOR_TTL,
            };
            now.duration_since(slot.arrived_at) < ttl
        });
        // A watermark only has to outlive the datagrams that could still be in
        // flight when it was written.
        self.retired
            .retain(|_, (_, _, at)| now.duration_since(*at) < deadline::CARET_GRACE);
        before - self.slots.len()
    }

    /// Everything currently believed about a scope.
    pub fn get(&self, scope: &Target, kind: TransientKind) -> Option<&TransientSlot> {
        self.slots.get(&(scope.clone(), kind as u8))
    }

    /// Drop everything a session held. What a disconnect does.
    pub fn forget_session(&mut self, connection_epoch: &[u8; 16]) -> usize {
        let before = self.slots.len();
        self.slots
            .retain(|_, slot| &slot.connection_epoch != connection_epoch);
        before - self.slots.len()
    }
}

impl Default for TransientStore {
    fn default() -> Self {
        Self::new()
    }
}

/// What a client says on the Live plane's control lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveControl {
    /// Watch these scopes. Replaces the connection's subscription set.
    Subscribe { scopes: Vec<Target> },
    /// Stop watching, and do not let anything already in flight undo it.
    Retire { scope: Target, seq: u64 },
}

impl LiveControl {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard live control")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_TRANSIENT_ITEM_BYTES * 8 {
            return Err(Invalid::TooLarge);
        }
        let control: Self = postcard::from_bytes(bytes).map_err(|_| Invalid::Malformed)?;
        if control.encode() != bytes {
            return Err(Invalid::NonCanonical);
        }
        control.validate()?;
        Ok(control)
    }

    pub fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Subscribe { scopes } => {
                if scopes.len() > slots::MAX_SUBSCRIBED_SCOPES_PER_CONNECTION {
                    return Err(Invalid::Bounds);
                }
                for scope in scopes {
                    scope.validate()?;
                }
                Ok(())
            }
            Self::Retire { scope, .. } => scope.validate(),
        }
    }
}

/// How long a caret is held before it is sent, exposed so a client and the
/// Station agree without either mirroring a literal.
pub const CURSOR_COALESCE: Duration = deadline::CURSOR_COALESCE;
