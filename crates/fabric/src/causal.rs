//! lait's causal contract over the collaborative algebra.
//!
//! Nothing here names a Loro type. What crosses this seam is a head set, an
//! artifact, and an anchor — all lait's own, all bounded, all encodable.
//!
//! **A version is a head set, not a version vector.** A version vector grows by
//! one dead entry per writable activation, forever, because a fresh peer id is
//! minted each time; `crates/fabric/tests/causal_evidence.rs` measures exactly
//! that — 128 entries after 128 activations, against a head set that stays at
//! one. A head set is sized by concurrency, not by lifetime, so it is bounded
//! by construction.
//!
//! **And a version is not a sync input.** The same evidence shows a head set
//! cannot be expanded by a replica that has not seen the operations it names,
//! which rules out "exchange versions, compute a delta" between diverged peers
//! and rules in something better: Bodies converge by exchanging
//! content-addressed artifacts, imported in any order, with causally incomplete
//! ones held pending until their dependencies arrive. No causal summary crosses
//! the wire at all. [`Version`] exists for ordering, anchors, and
//! staleness — never as the thing a peer is asked to interpret.

use loro::{Frontiers, LoroDoc, ID};
use serde::{Deserialize, Serialize};

/// Maximum heads in one [`Version`]. A head set is one entry per
/// concurrent writer at a moment; this is the protocol's refusal point, far
/// above any real concurrency and far below anything that could exhaust a
/// receiver.
pub const MAX_HEADS: usize = 256;

/// The encoded generation of the causal artifact formats.
pub const CAUSAL_FORMAT_VERSION: u8 = 1;

/// One operation's identity: who wrote it and where in their sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpHead {
    pub writer: u64,
    pub sequence: i32,
}

/// A position in the collaborative history: the set of operations nothing else
/// depends on yet.
///
/// Canonical — sorted and deduplicated — so two replicas at the same position
/// encode identical bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub format_version: u8,
    pub heads: Vec<OpHead>,
}

/// How two versions relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausalRelation {
    Equal,
    /// The first includes everything the second does, and more.
    Dominates,
    Dominated,
    /// Neither includes the other.
    Concurrent,
    /// This replica has not seen enough history to say. Returned rather than
    /// guessed: reporting `Concurrent` for a version we simply have not
    /// received would be a lie shaped exactly like the truth.
    Undetermined,
}

/// Why a causal operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CausalError {
    /// No collaborative Body at this key.
    NotCollaborative,
    /// A version, artifact, or path exceeded a protocol bound.
    Bounds,
    /// Non-canonical encoding.
    NonCanonical,
    /// The base a delta was requested from is not in this replica's history.
    MissingBase,
    /// The work predates the Body's published retention frontier, so a
    /// compacted document cannot admit it.
    ///
    /// This is a refusal, not a loss: the frontier is named so a writer knows
    /// what happened, and recovery is to rebuild from the archive taken before
    /// the trim, or to re-bootstrap. §5.2's outcome 2 is only acceptable
    /// because the refusal is typed, attributable, and recoverable.
    BeforeRetentionFrontier { frontier: Version },
    /// The engine refused an operation.
    Engine(String),
}

impl std::fmt::Display for CausalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CausalError::BeforeRetentionFrontier { .. } => write!(
                f,
                "this work predates the Body's retention frontier; rebuild from \
                 its archive or re-bootstrap"
            ),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for CausalError {}

impl Version {
    pub(crate) fn from_frontiers(frontiers: &Frontiers) -> Self {
        let mut heads: Vec<OpHead> = frontiers
            .iter()
            .map(|id| OpHead {
                writer: id.peer,
                sequence: id.counter,
            })
            .collect();
        heads.sort();
        heads.dedup();
        Self {
            format_version: CAUSAL_FORMAT_VERSION,
            heads,
        }
    }

    pub(crate) fn to_frontiers(&self) -> Frontiers {
        Frontiers::from_iter(self.heads.iter().map(|h| ID {
            peer: h.writer,
            counter: h.sequence,
        }))
    }

    /// The empty version, which every replica shares and every history starts
    /// from. Universally convertible, unlike any other.
    pub fn empty() -> Self {
        Self {
            format_version: CAUSAL_FORMAT_VERSION,
            heads: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.heads.is_empty()
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard Engine version")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, CausalError> {
        let version: Self = postcard::from_bytes(bytes).map_err(|_| CausalError::NonCanonical)?;
        version.validate()?;
        if version.encode() != bytes {
            return Err(CausalError::NonCanonical);
        }
        Ok(version)
    }

    pub fn validate(&self) -> Result<(), CausalError> {
        if self.format_version != CAUSAL_FORMAT_VERSION {
            return Err(CausalError::NonCanonical);
        }
        if self.heads.len() > MAX_HEADS {
            return Err(CausalError::Bounds);
        }
        for w in self.heads.windows(2) {
            if w[0] >= w[1] {
                return Err(CausalError::NonCanonical);
            }
        }
        Ok(())
    }
}

/// The artifacts a Body's material is made of.
///
/// Every one is content-addressed and imported independently. A receiver that
/// gets them out of order converges anyway, because the engine holds causally
/// incomplete work pending rather than rejecting it — which is what lets
/// convergence be a presence question rather than a causal negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Artifact {
    /// Operations after some base. What an ordinary edit produces.
    Delta {
        format_version: u8,
        base: Version,
        result: Version,
        bytes: Vec<u8>,
    },
    /// Current state with history trimmed at a retention frontier. What
    /// reconstructs a Body without replaying it.
    Checkpoint {
        format_version: u8,
        retention_frontier: Version,
        result: Version,
        bytes: Vec<u8>,
    },
    /// The complete history as it stood immediately before a trim.
    ///
    /// A snapshot rather than a range of updates, and that is measured rather
    /// than assumed: a document rebuilt from a complete archive admits
    /// pre-checkpoint work outright, while assembling one from ranges would
    /// have to reconstruct the same thing to get the same property.
    Archive {
        format_version: u8,
        result: Version,
        bytes: Vec<u8>,
    },
    /// An atomic Body's replacement value. Atomic Bodies implement the same
    /// contract with no CRDT delta.
    Replace { format_version: u8, bytes: Vec<u8> },
}

impl Artifact {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard Engine artifact")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, CausalError> {
        let artifact: Self = postcard::from_bytes(bytes).map_err(|_| CausalError::NonCanonical)?;
        if artifact.encode() != bytes {
            return Err(CausalError::NonCanonical);
        }
        Ok(artifact)
    }

    /// The version this artifact leaves a replica at, if it names one.
    pub fn result(&self) -> Option<&Version> {
        match self {
            Artifact::Delta { result, .. }
            | Artifact::Checkpoint { result, .. }
            | Artifact::Archive { result, .. } => Some(result),
            Artifact::Replace { .. } => None,
        }
    }

    /// The encoded payload's size — what quota projection reads before a
    /// transaction is applied rather than after.
    pub fn payload_len(&self) -> usize {
        match self {
            Artifact::Delta { bytes, .. }
            | Artifact::Checkpoint { bytes, .. }
            | Artifact::Archive { bytes, .. }
            | Artifact::Replace { bytes, .. } => bytes.len(),
        }
    }
}

/// What an import did.
///
/// `pending` is not a failure. An artifact whose dependencies have not arrived
/// is held, and applies itself when they do — which is exactly why artifacts
/// can be exchanged in any order and why no version negotiation is needed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportStatus {
    /// Whether anything new was applied.
    pub applied: bool,
    /// Whether some of the material is waiting on dependencies.
    pub pending: bool,
}

/// A position inside a collaborative value, and the version it was taken at.
///
/// Minted here because plan 14 consumes it and cannot mint it: a caret, a
/// comment attached to a text range, and a selection all name a position that
/// concurrent edits are moving underneath them, and only the algebra that moves
/// them can map one across versions.
///
/// An anchor is a value, not durable material. A World may persist one inside a
/// Body; the substrate neither roots them nor promises an old one stays
/// resolvable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub format_version: u8,
    /// Which Body this position is inside.
    ///
    /// Load-bearing, not bookkeeping. Every Body of one activation shares a
    /// writer id, so operation ids collide across documents — without this an
    /// anchor taken in one Body resolves against another to a plausible,
    /// silently wrong index, which is the one thing resolving must never
    /// produce.
    pub body: [u8; 32],
    /// The typed path within the Body, as the collaborative schema names it.
    pub path: String,
    /// The operation the position is attached to, if the algebra could bind one.
    /// Absent means the position is an offset from the start or end.
    pub anchored_to: Option<OpHead>,
    /// The offset at the time the anchor was taken.
    pub offset: u64,
    /// Which side of `anchored_to` the position sits on. An insertion exactly
    /// at a caret has to go somewhere, and this is what decides.
    pub after: bool,
    pub taken_at: Version,
}

/// The result of resolving an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorResolution {
    Resolved(u64),
    /// The position could not be mapped: the surrounding material was deleted,
    /// or the anchor predates what this replica retains.
    ///
    /// Total by design. A renderer may map a drifted anchor forward or hide it;
    /// what it must never get is a silently wrong index, and what resolving must
    /// never do is mutate the Body — so anchors are safe to resolve on a
    /// read-only replica.
    Drifted,
}

impl Anchor {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard Engine anchor")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, CausalError> {
        let anchor: Self = postcard::from_bytes(bytes).map_err(|_| CausalError::NonCanonical)?;
        anchor.taken_at.validate()?;
        if anchor.encode() != bytes {
            return Err(CausalError::NonCanonical);
        }
        Ok(anchor)
    }
}

/// Compare two positions in one replica's history.
pub(crate) fn relation(doc: &LoroDoc, a: &Version, b: &Version) -> CausalRelation {
    if a == b {
        return CausalRelation::Equal;
    }
    let (Some(a_vv), Some(b_vv)) = (
        doc.frontiers_to_vv(&a.to_frontiers()),
        doc.frontiers_to_vv(&b.to_frontiers()),
    ) else {
        // Expanding a head set needs the history it names. A replica that has
        // not received those operations cannot order them, and says so.
        return CausalRelation::Undetermined;
    };
    match (a_vv.includes_vv(&b_vv), b_vv.includes_vv(&a_vv)) {
        (true, true) => CausalRelation::Equal,
        (true, false) => CausalRelation::Dominates,
        (false, true) => CausalRelation::Dominated,
        (false, false) => CausalRelation::Concurrent,
    }
}

/// Checkpoint policy, deterministic from encoded sizes rather than wall time.
///
/// Wall time is not evidence anywhere in this substrate, and it would be
/// especially wrong here: two replicas that checkpointed on different clocks
/// would trim at different frontiers and refuse different work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPolicy {
    /// Deltas in the tail before a checkpoint replaces them.
    pub max_tail_deltas: usize,
    /// Encoded tail bytes before a checkpoint replaces them.
    pub max_tail_bytes: usize,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        // F0 measured an ordinary edit's delta at 105 bytes, so 256 of them
        // encode to about 33 KB — two orders under the byte threshold. The
        // count is therefore what bounds an ordinary Body, and the byte
        // threshold exists for the Body that pastes megabytes at a time. Both
        // are operator-lowerable; neither may be raised past a protocol
        // maximum, because a peer has to be able to receive the result.
        Self {
            max_tail_deltas: 256,
            max_tail_bytes: 8 * 1024 * 1024,
        }
    }
}

impl CheckpointPolicy {
    /// Whether a tail of this shape should be replaced by a checkpoint.
    pub fn should_checkpoint(&self, tail_deltas: usize, tail_bytes: usize) -> bool {
        tail_deltas >= self.max_tail_deltas || tail_bytes >= self.max_tail_bytes
    }
}

/// A reference to one protected, chunked, content-addressed artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub hash: [u8; 32],
    pub len: u64,
}

/// What a collaborative Body's head commits: enough to reconstruct current
/// state, plus a bounded commitment to the history behind it.
///
/// The split is the point. A snapshot conflates *active state size* with
/// *retained history*, so an ordinary edit paid for both. Here the checkpoint
/// reconstructs state, the tail carries what came after it, and archives are
/// reachable through an index root rather than listed — so the descriptor stays
/// the same size however long the Body lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Material {
    pub format_version: u8,
    /// Reconstructs current state on its own.
    pub checkpoint: ArtifactRef,
    /// Updates after the checkpoint, in order. Bounded by [`CheckpointPolicy`].
    pub delta_tail: Vec<ArtifactRef>,
    /// Root of the index mapping each retention frontier this Body has passed
    /// to the archive taken immediately before that trim. A root rather than a
    /// list, so the descriptor does not grow with retained history.
    pub history_root: Option<[u8; 32]>,
    pub history_count: u64,
    pub version: Version,
}

impl Material {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard body material")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, CausalError> {
        let material: Self = postcard::from_bytes(bytes).map_err(|_| CausalError::NonCanonical)?;
        material.validate()?;
        if material.encode() != bytes {
            return Err(CausalError::NonCanonical);
        }
        Ok(material)
    }

    pub fn validate(&self) -> Result<(), CausalError> {
        if self.format_version != CAUSAL_FORMAT_VERSION {
            return Err(CausalError::NonCanonical);
        }
        if self.delta_tail.len() > CheckpointPolicy::default().max_tail_deltas {
            return Err(CausalError::Bounds);
        }
        self.version.validate()
    }

    /// Encoded tail bytes — the input to the checkpoint decision, and to quota
    /// projection before a transaction is applied rather than after.
    pub fn tail_bytes(&self) -> u64 {
        self.delta_tail.iter().map(|r| r.len).sum()
    }
}
