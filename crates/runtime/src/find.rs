//! Bounded reads over World-declared vocabulary.
//!
//! Find composes read intent; it does not mutate a World, start durable work,
//! choose a backend, or grant authority. Runtime later evaluates a typed
//! `find::Query` through an ordinary `Session` or an Attempt-bound facade. This
//! Queries and continuations are pinned to exact immutable World publications;
//! the same evaluator and disclosure rules serve interactive and agent reads.
//!
//! Every ranked flow is a total order. Runtime appends canonical [`NodeId`] as
//! the final ascending tie-break, including for ranked `Seek` and `Merge`
//! outputs; a Query cannot expose an unordered flow as its final output.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use mechanics::{
    ids::{ActorId, DeviceId, SpaceId},
    station::Epoch,
};
use replica::{
    body::{BodyKey, SchemaId, WorldId},
    frontier::AuthorityFrontier,
};
use serde::{Deserialize, Serialize};

/// Standalone canonical encoding version for [`Grant`].
const GRANT_VERSION: u8 = 1;
/// Standalone canonical encoding version for [`Query`].
const QUERY_VERSION: u8 = 4;
/// Canonical encoding version for one Runtime-issued [`Cursor`].
const CURSOR_VERSION: u8 = 4;
/// Canonical encoding version for one descriptor-embedded [`Schema`].
///
/// Descriptor section tag `0x0003` accepts exactly this value. A different
/// Schema grammar requires a new section tag; this constant is not an
/// in-section negotiation point.
const SCHEMA_VERSION: u8 = 1;

/// Maximum canonical bytes for one standalone Grant.
pub const MAX_GRANT_BYTES: usize = 65_536;
/// Maximum Schemas one Grant may name.
pub const MAX_SCHEMAS_PER_GRANT: usize = 64;
/// Maximum Fields one Grant may name.
pub const MAX_FIELDS_PER_GRANT: usize = 1_024;
/// Maximum Edges one Grant may name.
pub const MAX_EDGES_PER_GRANT: usize = 1_024;
/// Maximum Gates one Grant may name.
pub const MAX_GATES_PER_GRANT: usize = 256;
/// Maximum optional Features one Grant may name.
pub const MAX_FEATURES_PER_GRANT: usize = 256;
/// Maximum canonical bytes for one World-declared Find Schema.
pub const MAX_SCHEMA_BYTES: usize = 262_144;
/// Maximum Body sources one Find Schema may declare.
pub const MAX_SOURCES_PER_SCHEMA: usize = 64;
/// Maximum analyzers one Find Schema may declare.
pub const MAX_ANALYZERS_PER_SCHEMA: usize = 256;
/// Maximum canonical bytes for one standalone Query.
pub const MAX_QUERY_BYTES: usize = 262_144;
/// Maximum Steps in one Query DAG.
pub const MAX_QUERY_STEPS: usize = 256;
/// Maximum inputs to one Query Step.
pub const MAX_STEP_INPUTS: usize = 32;
/// Maximum opaque bytes in one cursor.
pub const MAX_CURSOR_BYTES: usize = 4_096;
/// Maximum rows returned by one page. Work bounds remain independent security
/// ceilings and include every posting or hidden row scanned to produce it.
pub const MAX_PAGE_SIZE: u32 = 10_000;
/// Maximum stable node identities in one Id Seek.
pub const MAX_SEEK_IDS: usize = 256;
/// Maximum durable sources in one Body seek. This matches Replica's maximum
/// transaction fan-out so one committed change can be projected in one Find.
pub const MAX_SEEK_BODIES: usize = 4_096;
/// Maximum World-defined identity bytes for one node.
pub const MAX_NODE_ID_BYTES: usize = 256;
/// Maximum predicates in one Keep operation.
pub const MAX_KEEP_PREDICATES: usize = 64;
/// Maximum Fields, Edges, or ranking methods named by one operation.
pub const MAX_REFS_PER_OP: usize = 64;
/// Maximum text or opaque probe bytes in one operation.
pub const MAX_OPERAND_BYTES: usize = 65_536;
const GRANT_DIGEST_DOMAIN: &[u8] = b"lait/find/grant/1\0";
const QUERY_DIGEST_DOMAIN: &[u8] = b"lait/find/query/1\0";

/// One version of a World-owned Find Schema.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaRef {
    pub name: SchemaId,
    pub version: u32,
}

macro_rules! named_ref {
    ($name:ident, $summary:literal) => {
        #[doc = $summary]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name {
            /// The Find Schema that owns the name.
            pub schema: SchemaRef,
            /// The declared name, interpreted only by that World.
            pub name: SchemaId,
        }
    };
}

named_ref!(FieldRef, "A Field declared by one Find Schema version.");
named_ref!(EdgeRef, "An Edge declared by one Find Schema version.");
named_ref!(
    GateRef,
    "A disclosure Gate declared by one Find Schema version."
);
named_ref!(
    FeatureRef,
    "An optional feature channel declared by one Find Schema version."
);
named_ref!(
    AnalyzerRef,
    "An analyzer declared by one Find Schema version."
);

/// One Body Schema version from which an extractor derives Find nodes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub name: SchemaId,
    pub version: u32,
}

/// Exact scalar semantics for a declared Field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FieldKind {
    Bool,
    Signed,
    Unsigned,
    Bytes,
    Text,
}

/// One exact Field declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub reference: FieldRef,
    pub kind: FieldKind,
    /// The analyzer used for term matching. `None` means the Field supports
    /// exact predicates only.
    pub analyzer: Option<AnalyzerRef>,
}

/// One exact traversal declaration. The Gate is evaluated before the Edge may
/// influence traversal, ranking, counts, cursors, or packing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub reference: EdgeRef,
    pub target: SchemaRef,
    pub gate: GateRef,
}

/// One disclosure Gate and its canonical authority demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub reference: GateRef,
    pub demand: Vec<u8>,
}

/// One analyzer implementation and its identity-bearing configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Analyzer {
    pub reference: AnalyzerRef,
    pub configuration: Vec<u8>,
}

/// One optional augmented feature channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feature {
    pub reference: FeatureRef,
    /// Identity of the feature implementation/configuration. Answers carry the
    /// actual feature stamp used for one evaluation.
    pub stamp: [u8; 32],
}

/// One versioned, World-owned Find vocabulary.
///
/// Every collection is a canonical set sorted by its public reference. The
/// complete declaration is implementation-identity material: source, Field,
/// Edge, Gate, analyzer, feature, operator, mode, or Bound changes move the
/// declaring World's implementation id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    pub reference: SchemaRef,
    pub sources: Vec<SourceRef>,
    pub fields: Vec<Field>,
    pub edges: Vec<Edge>,
    pub gates: Vec<Gate>,
    pub analyzers: Vec<Analyzer>,
    pub features: Vec<Feature>,
    pub ops: OpSet,
    pub modes: ModeSet,
    pub bound: Bound,
}

/// The package coordinate and executable contract of one declared extractor.
///
/// `semantic_digest` commits to the extractor implementation/artifact, not
/// merely its source and output schemas. `abi_version` commits to the Runtime ↔
/// World extraction call/row contract. A package activation which changes
/// extraction semantics must therefore move corpus identity even if its Body
/// and Find schema declarations are byte-identical.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Extractor {
    pub schema: SchemaRef,
    pub source: SourceRef,
    pub abi_version: u16,
    pub semantic_digest: [u8; 32],
}

pub const EXTRACTOR_ABI_VERSION: u16 = 1;

/// One exact scalar emitted by a World extractor.
///
/// Variable-width values are reference counted so the immutable row and its
/// postings can share storage across publications.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Value {
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Bytes(Arc<[u8]>),
    Text(Arc<str>),
}

impl Value {
    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(Arc::from(value.into()))
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(Arc::from(value.into()))
    }
}

/// Stable identity of one extracted node inside a Find Schema version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeKey {
    pub schema: SchemaRef,
    pub node: NodeId,
}

/// One disclosed Field value and the analyzer terms derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedField {
    pub reference: FieldRef,
    pub value: Value,
    pub gate: Option<GateRef>,
    /// Canonical analyzer output. Runtime treats these bytes as opaque.
    pub terms: Vec<Arc<[u8]>>,
}

/// One outward Edge and its already-resolved stable targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedEdge {
    pub reference: EdgeRef,
    pub gate: GateRef,
    pub targets: Vec<NodeKey>,
}

/// One optional augmented feature value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedFeature {
    pub reference: FeatureRef,
    pub gate: Option<GateRef>,
    pub value: Arc<[u8]>,
}

/// One complete node emitted by a World extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedNode {
    pub key: NodeKey,
    pub gate: Option<GateRef>,
    pub fields: Vec<ExtractedField>,
    pub edges: Vec<ExtractedEdge>,
    pub features: Vec<ExtractedFeature>,
}

/// Complete replacement extraction for one readable Body image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyExtraction {
    pub body: BodyKey,
    pub stamp: Vec<u8>,
    pub nodes: Vec<ExtractedNode>,
}

/// A stable World-defined node identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(Vec<u8>);

impl NodeId {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Invalid> {
        if bytes.is_empty() || bytes.len() > MAX_NODE_ID_BYTES {
            return Err(Invalid::InvalidOperand("node id"));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn is_valid(&self) -> bool {
        !self.0.is_empty() && self.0.len() <= MAX_NODE_ID_BYTES
    }
}

/// One nonzero canonical Step identity. Valid Queries use the contiguous ids
/// `1..=steps.len()` in topological order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StepId(u32);

impl StepId {
    pub const fn new(raw: u32) -> Option<Self> {
        if raw == 0 {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// An opaque cursor returned by Runtime and bound to read coordinates.
///
/// Callers may carry these bytes and send them back, but cannot supply their
/// own coordinates. Runtime decodes the canonical envelope and compares every
/// semantic and authority coordinate after deriving the current ambient
/// prefix. A mismatch refuses the request before evaluator or corpus access.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cursor(Vec<u8>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CursorEnvelope {
    coordinates: Coordinates,
    /// Evaluator-owned position bytes. Their grammar belongs to the evaluator,
    /// while this envelope binds them to the request that produced them.
    position: Vec<u8>,
}

impl Cursor {
    /// Parse Runtime-issued opaque cursor bytes.
    pub fn new(bytes: Vec<u8>) -> Result<Self, Invalid> {
        let cursor = Self(bytes);
        cursor.decode_canonical()?;
        Ok(cursor)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Issue a cursor from Runtime-derived coordinates and the evaluator's
    /// canonical continuation position.
    pub(crate) fn issue(
        coordinates: &Coordinates,
        query: &Query,
        position: Vec<u8>,
    ) -> Result<Self, Invalid> {
        if position.is_empty() {
            return Err(Invalid::InvalidCursor);
        }
        let mut coordinates = coordinates.clone();
        coordinates.query = query.cursor_query_digest()?;
        let bytes = postcard::to_stdvec(&(
            CURSOR_VERSION,
            CursorEnvelope {
                coordinates,
                position,
            },
        ))
        .map_err(|_| Invalid::InvalidCursor)?;
        Self::new(bytes)
    }

    fn decode_canonical(&self) -> Result<CursorEnvelope, Invalid> {
        if self.0.is_empty() || self.0.len() > MAX_CURSOR_BYTES {
            return Err(Invalid::InvalidCursor);
        }
        let (version, envelope): (u8, CursorEnvelope) =
            postcard::from_bytes(&self.0).map_err(|_| Invalid::InvalidCursor)?;
        if version != CURSOR_VERSION || envelope.position.is_empty() {
            return Err(Invalid::InvalidCursor);
        }
        let canonical =
            postcard::to_stdvec(&(version, &envelope)).map_err(|_| Invalid::InvalidCursor)?;
        if canonical != self.0 {
            return Err(Invalid::InvalidCursor);
        }
        Ok(envelope)
    }

    fn validate_for(&self, expected: &Coordinates, query: &Query) -> Result<(), Invalid> {
        let actual = self.decode_canonical()?.coordinates;
        let mut expected = expected.clone();
        expected.query = query.cursor_query_digest()?;

        macro_rules! same {
            ($field:ident) => {
                if actual.$field != expected.$field {
                    return Err(Invalid::CursorMismatch(stringify!($field)));
                }
            };
        }
        same!(epoch);
        same!(space);
        same!(world);
        same!(implementation);
        same!(root);
        same!(extractor_schema_digest);
        same!(materialization);
        same!(actor);
        same!(device);
        same!(authority_frontier);
        same!(query);
        same!(schema);
        Ok(())
    }

    pub(crate) fn position_for(
        &self,
        expected: &Coordinates,
        query: &Query,
    ) -> Result<Vec<u8>, Invalid> {
        self.validate_for(expected, query)?;
        Ok(self.decode_canonical()?.position)
    }

    /// Validate every request/principal coordinate that is safe to inspect
    /// before choosing a retained publication, then return the Runtime-issued
    /// coordinates that name that publication. Publication fields are checked
    /// by the caller against the exact installed package and retained cache.
    pub(crate) fn route_for(
        &self,
        expected: &Coordinates,
        query: &Query,
    ) -> Result<Coordinates, Invalid> {
        let actual = self.decode_canonical()?.coordinates;
        let mut expected = expected.clone();
        expected.query = query.cursor_query_digest()?;
        macro_rules! same {
            ($field:ident) => {
                if actual.$field != expected.$field {
                    return Err(Invalid::CursorMismatch(stringify!($field)));
                }
            };
        }
        same!(epoch);
        same!(space);
        same!(world);
        same!(actor);
        same!(device);
        same!(authority_frontier);
        same!(query);
        same!(schema);
        Ok(actual)
    }
}

/// Whether a Query uses only exact root-derived material or may use named
/// optional features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Exact,
    Augmented { missing: MissingFeature },
}

impl Mode {
    const fn set(self) -> ModeSet {
        match self {
            Self::Exact => ModeSet::EXACT,
            Self::Augmented { .. } => ModeSet::AUGMENTED,
        }
    }
}

/// What an augmented Query does when a named optional feature is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissingFeature {
    Refuse,
    Drop,
    Continue,
}

/// A typed scalar used by exact Field predicates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Atom {
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Bytes(Vec<u8>),
    Text(String),
}

impl Atom {
    fn is_valid(&self) -> bool {
        match self {
            Self::Bool(_) | Self::Signed(_) | Self::Unsigned(_) => true,
            Self::Bytes(bytes) => !bytes.is_empty() && bytes.len() <= MAX_OPERAND_BYTES,
            Self::Text(text) => !text.is_empty() && text.len() <= MAX_OPERAND_BYTES,
        }
    }
}

/// One declared Field comparison.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Predicate {
    pub field: FieldRef,
    pub test: Test,
    pub value: Atom,
}

/// Exact comparison operations interpreted against a declared Field type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Test {
    Equal,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Contains,
    Prefix,
}

/// Candidate production.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Seek {
    Source,
    /// Produce nodes extracted from these exact durable Bodies. This is the
    /// live-projection access path: it uses the same publication, gates,
    /// bounds, evaluator, and result rows as every other Find instead of
    /// re-entering a World-wide query after each commit.
    Bodies(Vec<BodyKey>),
    Ids(Vec<NodeId>),
    Field(Predicate),
    Term {
        field: FieldRef,
        text: String,
        kind: Term,
    },
    Feature {
        feature: FeatureRef,
        probe: Vec<u8>,
    },
}

/// Declared lexical matching behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Term {
    Token,
    Phrase,
    Prefix,
}

/// Constrain an existing typed value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keep {
    pub predicates: Vec<Predicate>,
}

/// Follow declared Edges under one disclosure Gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Walk {
    pub edges: Vec<EdgeRef>,
    pub direction: Direction,
    pub min_hops: u16,
    pub max_hops: u16,
    pub unique: Unique,
    pub order: WalkOrder,
    pub emit: Emit,
    pub gate: GateRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unique {
    Walk,
    Trail,
    Acyclic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalkOrder {
    Breadth,
    Depth,
    Shortest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Emit {
    Nodes,
    Edges,
    Paths,
}

/// Establish a total order over candidates.
///
/// The declared keys precede Runtime's canonical NodeId tie-break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rank {
    pub by: Vec<RankBy>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RankBy {
    Field(FieldRef),
    Term(FieldRef),
    Feature(FeatureRef),
    Distance,
}

/// Combine compatible ranked branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merge {
    pub method: MergeMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeMethod {
    Union,
    Intersection,
    ReciprocalRank,
}

/// Assemble bounded Context from one typed input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pack {
    pub fields: Vec<FieldRef>,
}

/// One typed Query operation. Declaration order is the canonical tag order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Seek(Seek),
    Keep(Keep),
    Walk(Walk),
    Rank(Rank),
    Merge(Merge),
    Pack(Pack),
}

/// The Find operators a Query may compose.
///
/// The raw byte is part of the canonical contract. Unknown bits reject rather
/// than becoming future privilege on an older Runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpSet(u8);

impl OpSet {
    pub const SEEK: Self = Self(0x01);
    pub const KEEP: Self = Self(0x02);
    pub const WALK: Self = Self(0x04);
    pub const RANK: Self = Self(0x08);
    pub const MERGE: Self = Self(0x10);
    pub const PACK: Self = Self(0x20);
    pub const ALL: Self = Self(0x3f);

    /// Construct a nonempty set containing only known operator bits.
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits != 0 && bits & !Self::ALL.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Combine known operator sets. Construction already proves the union is
    /// within [`Self::ALL`].
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Retain only operators present in both sets.
    pub const fn intersection(self, other: Self) -> Option<Self> {
        Self::from_bits(self.0 & other.0)
    }

    pub const fn contains_all(self, requested: Self) -> bool {
        self.0 & requested.0 == requested.0
    }

    const fn is_valid(self) -> bool {
        Self::from_bits(self.0).is_some()
    }
}

/// The result modes a Query may request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModeSet(u8);

impl ModeSet {
    pub const EXACT: Self = Self(0x01);
    pub const AUGMENTED: Self = Self(0x02);
    pub const ALL: Self = Self(0x03);

    /// Construct a nonempty set containing only known mode bits.
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits != 0 && bits & !Self::ALL.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersection(self, other: Self) -> Option<Self> {
        Self::from_bits(self.0 & other.0)
    }

    pub const fn contains_all(self, requested: Self) -> bool {
        self.0 & requested.0 == requested.0
    }

    const fn is_valid(self) -> bool {
        Self::from_bits(self.0).is_some()
    }
}

/// Finite ceilings on the work one Query may consume.
///
/// Every field is required. Zero and `u64::MAX` are reserved invalid values;
/// callers cannot encode "unbounded" by omission or a sentinel. Runtime and
/// Station policy may impose smaller absolute ceilings when the Query is
/// admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Bound {
    pub decoded_bodies: u64,
    pub postings_read: u64,
    pub edges_visited: u64,
    pub nodes_visited: u64,
    pub paths_retained: u64,
    pub candidates_per_branch: u64,
    pub score_evaluations: u64,
    pub projected_bytes: u64,
    pub packed_tokens: u64,
    pub wall_millis: u64,
}

impl Bound {
    /// Intersect two ceilings. No composition path may raise either input.
    pub fn intersection(self, other: Self) -> Self {
        Self {
            decoded_bodies: self.decoded_bodies.min(other.decoded_bodies),
            postings_read: self.postings_read.min(other.postings_read),
            edges_visited: self.edges_visited.min(other.edges_visited),
            nodes_visited: self.nodes_visited.min(other.nodes_visited),
            paths_retained: self.paths_retained.min(other.paths_retained),
            candidates_per_branch: self.candidates_per_branch.min(other.candidates_per_branch),
            score_evaluations: self.score_evaluations.min(other.score_evaluations),
            projected_bytes: self.projected_bytes.min(other.projected_bytes),
            packed_tokens: self.packed_tokens.min(other.packed_tokens),
            wall_millis: self.wall_millis.min(other.wall_millis),
        }
    }

    /// Whether this ceiling contains every requested unit in `candidate`.
    pub const fn contains(self, candidate: Self) -> bool {
        candidate.decoded_bodies <= self.decoded_bodies
            && candidate.postings_read <= self.postings_read
            && candidate.edges_visited <= self.edges_visited
            && candidate.nodes_visited <= self.nodes_visited
            && candidate.paths_retained <= self.paths_retained
            && candidate.candidates_per_branch <= self.candidates_per_branch
            && candidate.score_evaluations <= self.score_evaluations
            && candidate.projected_bytes <= self.projected_bytes
            && candidate.packed_tokens <= self.packed_tokens
            && candidate.wall_millis <= self.wall_millis
    }

    const fn is_finite(self) -> bool {
        finite(self.decoded_bodies)
            && finite(self.postings_read)
            && finite(self.edges_visited)
            && finite(self.nodes_visited)
            && finite(self.paths_retained)
            && finite(self.candidates_per_branch)
            && finite(self.score_evaluations)
            && finite(self.projected_bytes)
            && finite(self.packed_tokens)
            && finite(self.wall_millis)
    }
}

/// Local Station ceilings applied before a Query reaches an evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub bound: Bound,
}

impl Policy {
    pub fn validate(self) -> Result<(), Invalid> {
        if self.bound.is_finite() {
            Ok(())
        } else {
            Err(Invalid::InvalidBound)
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            bound: Bound {
                decoded_bodies: 10_000,
                postings_read: 100_000,
                edges_visited: 100_000,
                nodes_visited: 100_000,
                paths_retained: 10_000,
                candidates_per_branch: 10_000,
                score_evaluations: 100_000,
                projected_bytes: 8 * 1_024 * 1_024,
                packed_tokens: 32_768,
                wall_millis: 10_000,
            },
        }
    }
}

const fn finite(value: u64) -> bool {
    value != 0 && value != u64::MAX
}

impl Schema {
    /// The maximal evaluator Grant expressed by this declaration.
    pub fn grant(&self) -> Grant {
        Grant {
            schemas: vec![self.reference.clone()],
            ops: self.ops,
            fields: self
                .fields
                .iter()
                .map(|field| field.reference.clone())
                .collect(),
            edges: self
                .edges
                .iter()
                .map(|edge| edge.reference.clone())
                .collect(),
            gates: self
                .gates
                .iter()
                .map(|gate| gate.reference.clone())
                .collect(),
            modes: self.modes,
            features: self
                .features
                .iter()
                .map(|feature| feature.reference.clone())
                .collect(),
            bound: self.bound,
        }
    }

    /// Return the unique in-memory ordering used by the implementation
    /// descriptor. Duplicate references remain duplicates and are refused by
    /// [`Self::validate`].
    pub fn canonicalized(&self) -> Self {
        let mut schema = self.clone();
        schema.sources.sort();
        schema
            .fields
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        schema
            .edges
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        schema
            .gates
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        schema
            .analyzers
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        schema
            .features
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        schema
    }

    /// Validate one canonical World declaration without touching a corpus.
    pub fn validate(&self) -> Result<(), Invalid> {
        canonical_set(
            &self.sources,
            MAX_SOURCES_PER_SCHEMA,
            false,
            "schema sources",
        )?;
        canonical_by(
            &self.fields,
            MAX_FIELDS_PER_GRANT,
            true,
            "schema fields",
            |field| &field.reference,
        )?;
        canonical_by(
            &self.edges,
            MAX_EDGES_PER_GRANT,
            true,
            "schema edges",
            |edge| &edge.reference,
        )?;
        canonical_by(
            &self.gates,
            MAX_GATES_PER_GRANT,
            true,
            "schema gates",
            |gate| &gate.reference,
        )?;
        canonical_by(
            &self.analyzers,
            MAX_ANALYZERS_PER_SCHEMA,
            true,
            "schema analyzers",
            |analyzer| &analyzer.reference,
        )?;
        canonical_by(
            &self.features,
            MAX_FEATURES_PER_GRANT,
            true,
            "schema features",
            |feature| &feature.reference,
        )?;
        if !self.ops.is_valid() {
            return Err(Invalid::InvalidOps);
        }
        if !self.modes.is_valid() {
            return Err(Invalid::InvalidModes);
        }
        if !self.bound.is_finite() {
            return Err(Invalid::InvalidBound);
        }

        for field in &self.fields {
            require_schema(&field.reference.schema, &self.reference, "declared field")?;
            if let Some(analyzer) = &field.analyzer {
                require_schema(&analyzer.schema, &self.reference, "field analyzer")?;
                if field.kind != FieldKind::Text
                    || self
                        .analyzers
                        .binary_search_by(|declared| declared.reference.cmp(analyzer))
                        .is_err()
                {
                    return Err(Invalid::InvalidOperand("field analyzer"));
                }
            }
        }
        for edge in &self.edges {
            require_schema(&edge.reference.schema, &self.reference, "declared edge")?;
            require_schema(&edge.gate.schema, &self.reference, "edge gate")?;
            if self
                .gates
                .binary_search_by(|declared| declared.reference.cmp(&edge.gate))
                .is_err()
            {
                return Err(Invalid::InvalidOperand("edge gate"));
            }
        }
        for gate in &self.gates {
            require_schema(&gate.reference.schema, &self.reference, "declared gate")?;
            if gate.demand.is_empty()
                || gate.demand.len() > MAX_OPERAND_BYTES
                || mechanics::authorization::AuthorizationDemand::decode_canonical(&gate.demand)
                    .is_err()
            {
                return Err(Invalid::InvalidOperand("gate demand"));
            }
        }
        for analyzer in &self.analyzers {
            require_schema(
                &analyzer.reference.schema,
                &self.reference,
                "declared analyzer",
            )?;
            if analyzer.configuration.len() > MAX_OPERAND_BYTES {
                return Err(Invalid::InvalidOperand("analyzer configuration"));
            }
        }
        for feature in &self.features {
            require_schema(
                &feature.reference.schema,
                &self.reference,
                "declared feature",
            )?;
        }
        Ok(())
    }

    /// Encode one canonical descriptor entry.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes =
            postcard::to_stdvec(&(SCHEMA_VERSION, self)).map_err(|_| Invalid::NonCanonical)?;
        if bytes.len() > MAX_SCHEMA_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    /// Strictly decode one descriptor entry.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_SCHEMA_BYTES {
            return Err(Invalid::TooLarge);
        }
        let (version, schema): (u8, Self) =
            postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        if version != SCHEMA_VERSION {
            return Err(Invalid::UnsupportedVersion(version));
        }
        schema.validate()?;
        if schema.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(schema)
    }
}

/// One node in the canonical typed Query DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: StepId,
    pub input: Vec<StepId>,
    pub op: Op,
    pub bound: Bound,
}

/// One bounded, typed read request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub schema: SchemaRef,
    /// Exact historical interpretation to read. `None` selects the Session's
    /// current publication. A root by itself is intentionally not accepted:
    /// the same Body Manifest can have different meaning under another World
    /// implementation or extractor contract.
    pub publication: Option<crate::publication::PublicationId>,
    pub mode: Mode,
    pub steps: Vec<Step>,
    pub output: StepId,
    pub bound: Bound,
    /// Requested result rows for this page. This is a semantic query input and
    /// therefore part of the cursor-bound query digest.
    pub page_size: u32,
    pub cursor: Option<Cursor>,
}

/// A commitment to canonical standalone Query bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct QueryDigest([u8; 32]);

impl QueryDigest {
    pub const fn from_bytes(raw: [u8; 32]) -> Self {
        Self(raw)
    }

    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Runtime-derived coordinates stamped onto every successful Answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coordinates {
    pub epoch: Epoch,
    pub space: SpaceId,
    pub world: WorldId,
    pub implementation: [u8; 32],
    pub root: [u8; 32],
    /// The declaration and exact source bindings that produced the corpus.
    pub extractor_schema_digest: crate::publication::ExtractorSchemaDigest,
    /// Station-local readable-material identity. This moves even when `root`
    /// does not if authority/key arrival makes opaque material readable.
    pub materialization: crate::publication::MaterializationId,
    pub actor: ActorId,
    pub device: DeviceId,
    pub authority_frontier: AuthorityFrontier,
    pub query: QueryDigest,
    pub schema: SchemaRef,
}

impl Coordinates {
    /// Portable semantic identity of the corpus used by this answer.
    pub const fn publication(&self) -> crate::publication::PublicationId {
        crate::publication::PublicationId::new(
            self.root,
            self.implementation,
            self.extractor_schema_digest,
        )
    }

    /// Complete Station-local identity, including same-root hydration changes.
    pub const fn world_publication(&self) -> crate::publication::WorldPublicationId {
        crate::publication::WorldPublicationId::new(self.publication(), self.materialization)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultField {
    pub reference: FieldRef,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultHop {
    pub edge: EdgeRef,
    pub from: NodeKey,
    pub to: NodeKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultPath {
    pub nodes: Vec<NodeKey>,
    pub hops: Vec<ResultHop>,
}

/// One stable result row. `fields` is empty unless the terminal Pack selected
/// fields; `path` is present when traversal provenance was retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultRow {
    /// Durable source of this extracted node. It lets a bounded Body seek
    /// preserve change grouping without a second lookup or product cache.
    pub source: BodyKey,
    pub key: NodeKey,
    pub fields: Vec<ResultField>,
    pub path: Option<ResultPath>,
}

/// One admitted, bounded Find result over an exact World publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Answer {
    coordinates: Coordinates,
    rows: Vec<ResultRow>,
    usage: Bound,
    next_cursor: Option<Cursor>,
    /// Exact admitted cardinality when the terminal direct posting can answer
    /// from gate-partition metadata. `None` is a typed unavailability signal;
    /// callers must never infer a total from one page's row count.
    matched_total: Option<u64>,
}

impl Answer {
    pub fn coordinates(&self) -> &Coordinates {
        &self.coordinates
    }

    pub fn rows(&self) -> &[ResultRow] {
        &self.rows
    }

    pub const fn usage(&self) -> Bound {
        self.usage
    }

    pub fn next_cursor(&self) -> Option<&Cursor> {
        self.next_cursor.as_ref()
    }

    pub const fn matched_total(&self) -> Option<u64> {
        self.matched_total
    }
}

/// The single post-lock entry to Find validation and evaluation.
pub(crate) struct Admission {
    pub query: Query,
    pub coordinates: Coordinates,
    pub policy: Policy,
    pub snapshot: Arc<replica::ReadSnapshot>,
    pub corpus: Arc<crate::corpus::Corpus>,
    pub gates: crate::find_evaluator::GrantedGates,
}

/// Validate the admitted request against local policy before evaluator access.
///
/// Every page is evaluated against one immutable publication. Supported
/// ordered pipelines return an opaque continuation. Rank/merge/walk pipelines
/// refuse with [`Failure::PaginationUnsupported`] if their output exceeds one
/// page, until their operator-specific state can be resumed without rebuilding
/// the hidden prefix; a partial answer is never returned without a cursor.
pub(crate) fn evaluate(admission: Admission) -> Result<Answer, Failure> {
    admission.query.validate()?;
    admission.policy.validate()?;
    if !admission.policy.bound.contains(admission.query.bound) {
        return Err(Failure::PolicyExceeded);
    }
    if let Some(cursor) = &admission.query.cursor {
        match cursor.validate_for(&admission.coordinates, &admission.query) {
            Ok(()) => {}
            Err(Invalid::CursorMismatch("materialization")) => {
                return Err(Failure::PublicationExpired);
            }
            Err(invalid) => return Err(Failure::Invalid(invalid)),
        }
    }
    if admission.corpus.coordinate() != admission.coordinates.world_publication() {
        return Err(Failure::Unavailable);
    }
    struct ByteTokens;
    impl crate::find_evaluator::TokenCounter for ByteTokens {
        fn count(&self, bytes: &[u8]) -> Result<u64, &'static str> {
            Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        }
    }

    let cursor_position = admission
        .query
        .cursor
        .as_ref()
        .map(|cursor| cursor.position_for(&admission.coordinates, &admission.query))
        .transpose()?;
    let evaluated = crate::find_evaluator::evaluate(crate::find_evaluator::Evaluation {
        query: &admission.query,
        corpus: &admission.corpus,
        gates: &admission.gates,
        admitted_bound: admission.policy.bound.intersection(admission.query.bound),
        cursor_position,
        feature_scorer: None,
        token_counter: &ByteTokens,
    })
    .map_err(|failure| match failure {
        crate::find_evaluator::Failure::Invalid(invalid) => Failure::Invalid(invalid),
        crate::find_evaluator::Failure::BoundExceeded(_) => Failure::PolicyExceeded,
        crate::find_evaluator::Failure::ContinuationUnavailable => Failure::PaginationUnsupported,
        crate::find_evaluator::Failure::FeatureUnavailable(_)
        | crate::find_evaluator::Failure::DistanceUnavailable
        | crate::find_evaluator::Failure::FeatureFailed(_, _)
        | crate::find_evaluator::Failure::TokenCounting(_)
        | crate::find_evaluator::Failure::MissingStep(_)
        | crate::find_evaluator::Failure::WrongFlow(_) => Failure::Unavailable,
    })?;

    let rows = evaluator_rows(evaluated.output, &admission.corpus)?;
    let next_cursor = evaluated
        .next_position
        .map(|position| Cursor::issue(&admission.coordinates, &admission.query, position))
        .transpose()?;
    let _snapshot = admission.snapshot;
    Ok(Answer {
        coordinates: admission.coordinates,
        rows,
        usage: evaluated.usage,
        next_cursor,
        matched_total: evaluated.matched_total,
    })
}

fn evaluator_rows(
    output: crate::find_evaluator::Output,
    corpus: &crate::corpus::Corpus,
) -> Result<Vec<ResultRow>, Failure> {
    use crate::find_evaluator::Output;
    let rows = match output {
        Output::Nodes(nodes) => nodes
            .into_iter()
            .map(|node| result_row(corpus, node.key, Vec::new(), None))
            .collect::<Result<Vec<_>, _>>()?,
        Output::Paths(paths) => paths
            .into_iter()
            .filter_map(|path| {
                let key = path.nodes.last()?.clone();
                Some(result_row(
                    corpus,
                    key,
                    Vec::new(),
                    Some(evaluator_path(path)),
                ))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Output::Ranked(nodes) => nodes
            .into_iter()
            .map(|node| result_row(corpus, node.key, Vec::new(), node.path.map(evaluator_path)))
            .collect::<Result<Vec<_>, _>>()?,
        Output::Context(nodes) => nodes
            .into_iter()
            .map(|node| {
                let fields = node
                    .fields
                    .into_iter()
                    .map(|field| ResultField {
                        reference: field.reference,
                        value: field.value,
                    })
                    .collect();
                result_row(corpus, node.key, fields, None)
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

fn result_row(
    corpus: &crate::corpus::Corpus,
    key: NodeKey,
    fields: Vec<ResultField>,
    path: Option<ResultPath>,
) -> Result<ResultRow, Failure> {
    let source = corpus.source(&key).ok_or(Failure::Unavailable)?;
    Ok(ResultRow {
        source,
        key,
        fields,
        path,
    })
}

fn evaluator_path(path: crate::find_evaluator::PathHit) -> ResultPath {
    ResultPath {
        nodes: path.nodes,
        hops: path
            .hops
            .into_iter()
            .map(|hop| ResultHop {
                edge: hop.edge,
                from: hop.from,
                to: hop.to,
            })
            .collect(),
    }
}

impl Query {
    /// Validate the complete request contract without reading corpus data.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.steps.is_empty() || self.steps.len() > MAX_QUERY_STEPS {
            return Err(Invalid::InvalidQuery("steps"));
        }
        if !(1..=MAX_PAGE_SIZE).contains(&self.page_size) {
            return Err(Invalid::InvalidQuery("page size"));
        }
        if u64::from(self.page_size) > self.bound.candidates_per_branch {
            return Err(Invalid::InvalidQuery("page size exceeds candidate bound"));
        }
        if !self.bound.is_finite() {
            return Err(Invalid::InvalidBound);
        }
        if let Some(cursor) = &self.cursor {
            cursor.decode_canonical()?;
        }

        let mut flows = BTreeMap::new();
        let mut steps = BTreeMap::new();
        for (position, step) in self.steps.iter().enumerate() {
            let ordinal = position
                .checked_add(1)
                .ok_or(Invalid::InvalidQuery("step id overflow"))?;
            let expected =
                u32::try_from(ordinal).map_err(|_| Invalid::InvalidQuery("step id overflow"))?;
            if step.id.get() != expected {
                return Err(Invalid::InvalidStep(step.id, "non-canonical id"));
            }
            canonical_set(&step.input, MAX_STEP_INPUTS, true, "step inputs")?;
            if !step.bound.is_finite() {
                return Err(Invalid::InvalidStep(step.id, "invalid bound"));
            }
            if !self.bound.contains(step.bound) {
                return Err(Invalid::InvalidStep(step.id, "bound exceeds query"));
            }
            if u64::from(self.page_size) > step.bound.candidates_per_branch {
                return Err(Invalid::InvalidStep(
                    step.id,
                    "page size exceeds candidate bound",
                ));
            }
            step.op.validate(&self.schema)?;
            if self.mode == Mode::Exact && step.op.uses_feature() {
                return Err(Invalid::InvalidStep(
                    step.id,
                    "feature requires augmented mode",
                ));
            }

            let mut input_flows = Vec::with_capacity(step.input.len());
            for input in &step.input {
                if *input >= step.id {
                    return Err(Invalid::InvalidStep(step.id, "input is not earlier"));
                }
                let flow = flows
                    .get(input)
                    .copied()
                    .ok_or(Invalid::InvalidStep(step.id, "missing input"))?;
                input_flows.push(flow);
            }
            let output = step
                .op
                .output(&input_flows)
                .map_err(|reason| Invalid::InvalidStep(step.id, reason))?;
            flows.insert(step.id, output);
            steps.insert(step.id, step);
        }

        if !steps.contains_key(&self.output) {
            return Err(Invalid::InvalidQuery("missing output"));
        }
        if !matches!(
            flows.get(&self.output),
            Some(Flow::Nodes | Flow::Ranked | Flow::Context)
        ) {
            return Err(Invalid::InvalidQuery("unstable output"));
        }

        let mut reachable = BTreeSet::new();
        let mut pending = vec![self.output];
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            let step = steps
                .get(&id)
                .ok_or(Invalid::InvalidQuery("missing reachable step"))?;
            pending.extend(step.input.iter().copied());
        }
        if reachable.len() != self.steps.len() {
            return Err(Invalid::InvalidQuery("unreachable step"));
        }

        Ok(())
    }

    /// Prove that this Query stays inside one validated Grant.
    pub fn validate_within(&self, grant: &Grant) -> Result<(), Invalid> {
        self.validate()?;
        grant.validate()?;
        granted(&self.schema, &grant.schemas, "schema")?;
        if !grant.modes.contains_all(self.mode.set()) {
            return Err(Invalid::NotGranted("mode"));
        }
        if !grant.bound.contains(self.bound) {
            return Err(Invalid::NotGranted("bound"));
        }
        for step in &self.steps {
            if !grant.bound.contains(step.bound) {
                return Err(Invalid::NotGranted("step bound"));
            }
            step.op.validate_within(grant)?;
        }
        Ok(())
    }

    /// Prove the complete Query is contained by its exact active declaration.
    pub fn validate_within_schema(&self, schema: &Schema) -> Result<(), Invalid> {
        if self.schema != schema.reference {
            return Err(Invalid::UndeclaredSchema("query schema"));
        }
        self.validate_within(&schema.grant())
    }

    /// Encode the validated Query to canonical standalone bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes =
            postcard::to_stdvec(&(QUERY_VERSION, self)).map_err(|_| Invalid::NonCanonical)?;
        if bytes.len() > MAX_QUERY_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    /// Decode one canonical standalone Query.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_QUERY_BYTES {
            return Err(Invalid::TooLarge);
        }
        let (version, query): (u8, Self) =
            postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        if version != QUERY_VERSION {
            return Err(Invalid::UnsupportedVersion(version));
        }
        query.validate()?;
        if query.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(query)
    }

    /// Commit to canonical Query bytes under the Find Query domain.
    pub fn digest(&self) -> Result<QueryDigest, Invalid> {
        let bytes = self.encode()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(QUERY_DIGEST_DOMAIN);
        hasher.update(&bytes);
        Ok(QueryDigest::from_bytes(*hasher.finalize().as_bytes()))
    }

    /// Digest the semantic request independently of its evaluator-owned cursor
    /// position. This is the stable query coordinate a cursor carries from one
    /// page to the next; the Answer still records the digest of the complete
    /// Query submitted for that page.
    fn cursor_query_digest(&self) -> Result<QueryDigest, Invalid> {
        let mut query = self.clone();
        query.cursor = None;
        query.digest()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Nodes,
    Paths,
    Ranked,
    Context,
}

impl Op {
    const fn set(&self) -> OpSet {
        match self {
            Self::Seek(_) => OpSet::SEEK,
            Self::Keep(_) => OpSet::KEEP,
            Self::Walk(_) => OpSet::WALK,
            Self::Rank(_) => OpSet::RANK,
            Self::Merge(_) => OpSet::MERGE,
            Self::Pack(_) => OpSet::PACK,
        }
    }

    fn validate(&self, schema: &SchemaRef) -> Result<(), Invalid> {
        match self {
            Self::Seek(seek) => validate_seek(seek, schema),
            Self::Keep(keep) => {
                canonical_set(
                    &keep.predicates,
                    MAX_KEEP_PREDICATES,
                    false,
                    "keep predicates",
                )?;
                for predicate in &keep.predicates {
                    validate_predicate(predicate, schema)?;
                }
                Ok(())
            }
            Self::Walk(walk) => {
                canonical_set(&walk.edges, MAX_REFS_PER_OP, false, "walk edges")?;
                require_schema(&walk.gate.schema, schema, "walk gate")?;
                for edge in &walk.edges {
                    require_schema(&edge.schema, schema, "walk edge")?;
                }
                if walk.max_hops == 0 || walk.min_hops > walk.max_hops {
                    return Err(Invalid::InvalidOperand("walk hops"));
                }
                Ok(())
            }
            Self::Rank(rank) => {
                canonical_set(&rank.by, MAX_REFS_PER_OP, false, "rank methods")?;
                for method in &rank.by {
                    match method {
                        RankBy::Field(field) | RankBy::Term(field) => {
                            require_schema(&field.schema, schema, "rank field")?;
                        }
                        RankBy::Feature(feature) => {
                            require_schema(&feature.schema, schema, "rank feature")?;
                        }
                        RankBy::Distance => {}
                    }
                }
                Ok(())
            }
            Self::Merge(_) => Ok(()),
            Self::Pack(pack) => {
                canonical_set(&pack.fields, MAX_REFS_PER_OP, false, "pack fields")?;
                for field in &pack.fields {
                    require_schema(&field.schema, schema, "pack field")?;
                }
                Ok(())
            }
        }
    }

    fn uses_feature(&self) -> bool {
        match self {
            Self::Seek(Seek::Feature { .. }) => true,
            Self::Rank(rank) => rank
                .by
                .iter()
                .any(|method| matches!(method, RankBy::Feature(_))),
            Self::Seek(_) | Self::Keep(_) | Self::Walk(_) | Self::Merge(_) | Self::Pack(_) => false,
        }
    }

    fn output(&self, input: &[Flow]) -> Result<Flow, &'static str> {
        match self {
            Self::Seek(seek) => {
                if !input.is_empty() {
                    return Err("Seek accepts no input");
                }
                match seek {
                    Seek::Term { .. } | Seek::Feature { .. } => Ok(Flow::Ranked),
                    Seek::Source | Seek::Bodies(_) | Seek::Ids(_) | Seek::Field(_) => {
                        Ok(Flow::Nodes)
                    }
                }
            }
            Self::Keep(_) => match input {
                [Flow::Nodes] => Ok(Flow::Nodes),
                [Flow::Paths] => Ok(Flow::Paths),
                [Flow::Ranked] => Ok(Flow::Ranked),
                _ => Err("Keep requires one Nodes, Paths, or Ranked input"),
            },
            Self::Walk(walk) => match input {
                [Flow::Nodes] => match walk.emit {
                    Emit::Nodes => Ok(Flow::Nodes),
                    Emit::Edges | Emit::Paths => Ok(Flow::Paths),
                },
                _ => Err("Walk requires one Nodes input"),
            },
            Self::Rank(_) => match input {
                [Flow::Nodes | Flow::Paths | Flow::Ranked] => Ok(Flow::Ranked),
                _ => Err("Rank requires one Nodes, Paths, or Ranked input"),
            },
            Self::Merge(_) => {
                if input.len() < 2 || input.iter().any(|flow| *flow != Flow::Ranked) {
                    Err("Merge requires at least two Ranked inputs")
                } else {
                    Ok(Flow::Ranked)
                }
            }
            Self::Pack(_) => match input {
                [Flow::Nodes | Flow::Ranked] => Ok(Flow::Context),
                _ => Err("Pack requires one Nodes or Ranked input"),
            },
        }
    }

    fn validate_within(&self, grant: &Grant) -> Result<(), Invalid> {
        if !grant.ops.contains_all(self.set()) {
            return Err(Invalid::NotGranted("operator"));
        }
        match self {
            Self::Seek(seek) => match seek {
                Seek::Source | Seek::Bodies(_) | Seek::Ids(_) => Ok(()),
                Seek::Field(predicate) => granted(&predicate.field, &grant.fields, "field"),
                Seek::Term { field, .. } => granted(field, &grant.fields, "field"),
                Seek::Feature { feature, .. } => granted(feature, &grant.features, "feature"),
            },
            Self::Keep(keep) => {
                for predicate in &keep.predicates {
                    granted(&predicate.field, &grant.fields, "field")?;
                }
                Ok(())
            }
            Self::Walk(walk) => {
                for edge in &walk.edges {
                    granted(edge, &grant.edges, "edge")?;
                }
                granted(&walk.gate, &grant.gates, "gate")
            }
            Self::Rank(rank) => {
                for method in &rank.by {
                    match method {
                        RankBy::Field(field) | RankBy::Term(field) => {
                            granted(field, &grant.fields, "field")?;
                        }
                        RankBy::Feature(feature) => {
                            granted(feature, &grant.features, "feature")?;
                        }
                        RankBy::Distance => {}
                    }
                }
                Ok(())
            }
            Self::Merge(_) => Ok(()),
            Self::Pack(pack) => {
                for field in &pack.fields {
                    granted(field, &grant.fields, "field")?;
                }
                Ok(())
            }
        }
    }
}

fn validate_seek(seek: &Seek, schema: &SchemaRef) -> Result<(), Invalid> {
    match seek {
        Seek::Source => Ok(()),
        Seek::Bodies(bodies) => canonical_set(bodies, MAX_SEEK_BODIES, false, "seek bodies"),
        Seek::Ids(ids) => canonical_set(ids, MAX_SEEK_IDS, false, "seek ids").and_then(|()| {
            if ids.iter().all(NodeId::is_valid) {
                Ok(())
            } else {
                Err(Invalid::InvalidOperand("node id"))
            }
        }),
        Seek::Field(predicate) => validate_predicate(predicate, schema),
        Seek::Term { field, text, kind } => {
            require_schema(&field.schema, schema, "term field")?;
            if text.is_empty() || text.len() > MAX_OPERAND_BYTES {
                return Err(Invalid::InvalidOperand("term"));
            }
            match kind {
                Term::Token => Ok(()),
                // Extractor terms currently carry no positions. Phrase is
                // refused until positional analyzer material exists.
                Term::Phrase => Err(Invalid::InvalidOperand("phrase term unsupported")),
                // A mature analyzed prefix needs a radix/FST dictionary with
                // bounded resumable expansion. Per-prefix postings amplify
                // memory and split UTF-8; do not pretend exact-token storage
                // provides prefix semantics.
                Term::Prefix => Err(Invalid::InvalidOperand("term prefix unsupported")),
            }
        }
        Seek::Feature { feature, probe } => {
            require_schema(&feature.schema, schema, "seek feature")?;
            if probe.is_empty() || probe.len() > MAX_OPERAND_BYTES {
                return Err(Invalid::InvalidOperand("feature probe"));
            }
            Ok(())
        }
    }
}

fn validate_predicate(predicate: &Predicate, schema: &SchemaRef) -> Result<(), Invalid> {
    require_schema(&predicate.field.schema, schema, "predicate field")?;
    if !predicate.value.is_valid() {
        return Err(Invalid::InvalidOperand("predicate value"));
    }
    Ok(())
}

fn require_schema(
    actual: &SchemaRef,
    expected: &SchemaRef,
    name: &'static str,
) -> Result<(), Invalid> {
    if actual != expected {
        return Err(Invalid::UndeclaredSchema(name));
    }
    Ok(())
}

fn granted<T: Ord>(value: &T, allowed: &[T], name: &'static str) -> Result<(), Invalid> {
    if allowed.binary_search(value).is_err() {
        return Err(Invalid::NotGranted(name));
    }
    Ok(())
}

/// A bounded envelope over World-declared Find vocabulary.
///
/// Collection fields are canonical sets represented as sorted, duplicate-free
/// vectors. A Grant contains no ambient coordinates or authority facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub schemas: Vec<SchemaRef>,
    pub ops: OpSet,
    pub fields: Vec<FieldRef>,
    pub edges: Vec<EdgeRef>,
    pub gates: Vec<GateRef>,
    pub modes: ModeSet,
    pub features: Vec<FeatureRef>,
    pub bound: Bound,
}

/// A commitment to the canonical standalone Grant bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GrantDigest([u8; 32]);

impl GrantDigest {
    pub const fn from_bytes(raw: [u8; 32]) -> Self {
        Self(raw)
    }

    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Why a Grant or Query contract was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    TooLarge,
    NonCanonical,
    UnsupportedVersion(u8),
    InvalidSet(&'static str),
    InvalidOps,
    InvalidModes,
    InvalidBound,
    InvalidCursor,
    CursorMismatch(&'static str),
    InvalidOperand(&'static str),
    InvalidQuery(&'static str),
    InvalidStep(StepId, &'static str),
    UndeclaredSchema(&'static str),
    NotGranted(&'static str),
    Widening(&'static str),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Invalid {}

/// Why an ordinary Find request produced no Answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Invalid(Invalid),
    Interrupted,
    PrincipalDenied,
    NoActiveImplementation,
    ImplementationUnavailable,
    AuthorityUnavailable(String),
    PolicyExceeded,
    PublicationUnavailable,
    /// A Station-local cursor outlived the retained immutable corpus it pinned.
    PublicationExpired,
    /// The requested page would be partial, but this operator topology has no
    /// honest resumable continuation yet.
    PaginationUnsupported,
    /// The Station's bounded cursor-lease table is full. Existing active
    /// continuations are preserved; the new query is refused rather than
    /// issuing a cursor that cannot be honored.
    CursorCapacityExceeded,
    /// The request was admitted, but this build has no declared evaluator.
    Unavailable,
}

impl From<Invalid> for Failure {
    fn from(value: Invalid) -> Self {
        Self::Invalid(value)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(invalid) => Some(invalid),
            _ => None,
        }
    }
}

impl Grant {
    /// Validate canonical sets, bounds, and reference containment.
    pub fn validate(&self) -> Result<(), Invalid> {
        canonical_set(&self.schemas, MAX_SCHEMAS_PER_GRANT, false, "schemas")?;
        canonical_set(&self.fields, MAX_FIELDS_PER_GRANT, true, "fields")?;
        canonical_set(&self.edges, MAX_EDGES_PER_GRANT, true, "edges")?;
        canonical_set(&self.gates, MAX_GATES_PER_GRANT, true, "gates")?;
        canonical_set(&self.features, MAX_FEATURES_PER_GRANT, true, "features")?;
        if !self.ops.is_valid() {
            return Err(Invalid::InvalidOps);
        }
        if !self.modes.is_valid() {
            return Err(Invalid::InvalidModes);
        }
        if !self.bound.is_finite() {
            return Err(Invalid::InvalidBound);
        }
        references_declared(&self.schemas, &self.fields, "fields")?;
        references_declared(&self.schemas, &self.edges, "edges")?;
        references_declared(&self.schemas, &self.gates, "gates")?;
        references_declared(&self.schemas, &self.features, "features")?;
        Ok(())
    }

    /// Prove this Grant is equal to or narrower than `parent`.
    pub fn validate_within(&self, parent: &Self) -> Result<(), Invalid> {
        self.validate()?;
        parent.validate()?;
        subset(&self.schemas, &parent.schemas, "schemas")?;
        subset(&self.fields, &parent.fields, "fields")?;
        subset(&self.edges, &parent.edges, "edges")?;
        subset(&self.gates, &parent.gates, "gates")?;
        subset(&self.features, &parent.features, "features")?;
        if !parent.ops.contains_all(self.ops) {
            return Err(Invalid::Widening("ops"));
        }
        if !parent.modes.contains_all(self.modes) {
            return Err(Invalid::Widening("modes"));
        }
        if !parent.bound.contains(self.bound) {
            return Err(Invalid::Widening("bound"));
        }
        Ok(())
    }

    /// Prove this Grant is contained by canonical active World declarations.
    ///
    /// Grant operator, mode, and work ceilings are global, so every Schema the
    /// Grant names must permit them. A caller that needs different ceilings or
    /// vocabularies for two Schemas declares two Grants.
    pub fn validate_within_schemas(&self, schemas: &[Schema]) -> Result<(), Invalid> {
        self.validate()?;
        if schemas
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.reference >= right.reference))
        {
            return Err(Invalid::InvalidSet("schema declarations"));
        }
        for schema in schemas {
            schema.validate()?;
        }

        for reference in &self.schemas {
            let schema = declared_schema(schemas, reference)?;
            if !schema.ops.contains_all(self.ops) {
                return Err(Invalid::NotGranted("operator"));
            }
            if !schema.modes.contains_all(self.modes) {
                return Err(Invalid::NotGranted("mode"));
            }
            if !schema.bound.contains(self.bound) {
                return Err(Invalid::NotGranted("bound"));
            }
        }
        for reference in &self.fields {
            let schema = declared_schema(schemas, &reference.schema)?;
            if schema
                .fields
                .binary_search_by(|field| field.reference.cmp(reference))
                .is_err()
            {
                return Err(Invalid::NotGranted("field"));
            }
        }
        for reference in &self.edges {
            let schema = declared_schema(schemas, &reference.schema)?;
            if schema
                .edges
                .binary_search_by(|edge| edge.reference.cmp(reference))
                .is_err()
            {
                return Err(Invalid::NotGranted("edge"));
            }
        }
        for reference in &self.gates {
            let schema = declared_schema(schemas, &reference.schema)?;
            if schema
                .gates
                .binary_search_by(|gate| gate.reference.cmp(reference))
                .is_err()
            {
                return Err(Invalid::NotGranted("gate"));
            }
        }
        for reference in &self.features {
            let schema = declared_schema(schemas, &reference.schema)?;
            if schema
                .features
                .binary_search_by(|feature| feature.reference.cmp(reference))
                .is_err()
            {
                return Err(Invalid::NotGranted("feature"));
            }
        }
        Ok(())
    }

    /// Encode the validated Grant to canonical standalone bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes =
            postcard::to_stdvec(&(GRANT_VERSION, self)).map_err(|_| Invalid::NonCanonical)?;
        if bytes.len() > MAX_GRANT_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    /// Decode one canonical standalone Grant.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_GRANT_BYTES {
            return Err(Invalid::TooLarge);
        }
        let (version, grant): (u8, Self) =
            postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        if version != GRANT_VERSION {
            return Err(Invalid::UnsupportedVersion(version));
        }
        grant.validate()?;
        if grant.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(grant)
    }

    /// Commit to the canonical Grant bytes under the Find Grant domain.
    pub fn digest(&self) -> Result<GrantDigest, Invalid> {
        let bytes = self.encode()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(GRANT_DIGEST_DOMAIN);
        hasher.update(&bytes);
        Ok(GrantDigest::from_bytes(*hasher.finalize().as_bytes()))
    }
}

fn declared_schema<'a>(
    schemas: &'a [Schema],
    reference: &SchemaRef,
) -> Result<&'a Schema, Invalid> {
    let index = schemas
        .binary_search_by(|schema| schema.reference.cmp(reference))
        .map_err(|_| Invalid::UndeclaredSchema("grant schema"))?;
    schemas
        .get(index)
        .ok_or(Invalid::UndeclaredSchema("grant schema"))
}

fn canonical_set<T: Ord>(
    values: &[T],
    maximum: usize,
    may_be_empty: bool,
    name: &'static str,
) -> Result<(), Invalid> {
    if values.len() > maximum || (!may_be_empty && values.is_empty()) {
        return Err(Invalid::InvalidSet(name));
    }
    if !values.windows(2).all(|pair| {
        let [left, right] = pair else {
            return false;
        };
        left < right
    }) {
        return Err(Invalid::InvalidSet(name));
    }
    Ok(())
}

fn canonical_by<T, K: Ord + ?Sized>(
    values: &[T],
    maximum: usize,
    may_be_empty: bool,
    name: &'static str,
    key: impl Fn(&T) -> &K,
) -> Result<(), Invalid> {
    if values.len() > maximum || (!may_be_empty && values.is_empty()) {
        return Err(Invalid::InvalidSet(name));
    }
    if !values.windows(2).all(|pair| {
        let [left, right] = pair else {
            return false;
        };
        key(left) < key(right)
    }) {
        return Err(Invalid::InvalidSet(name));
    }
    Ok(())
}

trait DeclaredRef: Ord {
    fn schema(&self) -> &SchemaRef;
}

macro_rules! declared_ref {
    ($name:ident) => {
        impl DeclaredRef for $name {
            fn schema(&self) -> &SchemaRef {
                &self.schema
            }
        }
    };
}

declared_ref!(FieldRef);
declared_ref!(EdgeRef);
declared_ref!(GateRef);
declared_ref!(FeatureRef);
declared_ref!(AnalyzerRef);

fn references_declared<T: DeclaredRef>(
    schemas: &[SchemaRef],
    values: &[T],
    name: &'static str,
) -> Result<(), Invalid> {
    if values
        .iter()
        .any(|value| schemas.binary_search(value.schema()).is_err())
    {
        return Err(Invalid::UndeclaredSchema(name));
    }
    Ok(())
}

fn subset<T: Ord>(candidate: &[T], parent: &[T], name: &'static str) -> Result<(), Invalid> {
    if candidate
        .iter()
        .any(|value| parent.binary_search(value).is_err())
    {
        return Err(Invalid::Widening(name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(name: &str, version: u32) -> SchemaRef {
        SchemaRef {
            name: SchemaId::parse(name).unwrap(),
            version,
        }
    }

    fn field(schema: &SchemaRef, name: &str) -> FieldRef {
        FieldRef {
            schema: schema.clone(),
            name: SchemaId::parse(name).unwrap(),
        }
    }

    fn bound(value: u64) -> Bound {
        Bound {
            decoded_bodies: value,
            postings_read: value,
            edges_visited: value,
            nodes_visited: value,
            paths_retained: value,
            candidates_per_branch: value,
            score_evaluations: value,
            projected_bytes: value,
            packed_tokens: value,
            wall_millis: value,
        }
    }

    fn with_bound_axis(mut bound: Bound, axis: usize, value: u64) -> Bound {
        match axis {
            0 => bound.decoded_bodies = value,
            1 => bound.postings_read = value,
            2 => bound.edges_visited = value,
            3 => bound.nodes_visited = value,
            4 => bound.paths_retained = value,
            5 => bound.candidates_per_branch = value,
            6 => bound.score_evaluations = value,
            7 => bound.projected_bytes = value,
            8 => bound.packed_tokens = value,
            9 => bound.wall_millis = value,
            _ => panic!("unknown Bound axis"),
        }
        bound
    }

    fn grant() -> Grant {
        let notes = schema("notes", 1);
        Grant {
            schemas: vec![notes.clone()],
            ops: OpSet::ALL,
            fields: vec![field(&notes, "title")],
            edges: Vec::new(),
            gates: Vec::new(),
            modes: ModeSet::EXACT,
            features: Vec::new(),
            bound: bound(1),
        }
    }

    fn query() -> Query {
        let notes = schema("notes", 1);
        Query {
            schema: notes.clone(),
            publication: None,
            mode: Mode::Exact,
            steps: vec![Step {
                id: StepId::new(1).unwrap(),
                input: Vec::new(),
                op: Op::Seek(Seek::Term {
                    field: field(&notes, "title"),
                    text: "q".to_owned(),
                    kind: Term::Token,
                }),
                bound: bound(1),
            }],
            output: StepId::new(1).unwrap(),
            bound: bound(1),
            page_size: 1,
            cursor: None,
        }
    }

    fn demand(capability: &str) -> Vec<u8> {
        mechanics::authorization::AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("com.example.notes", capability),
            mechanics::authorization::Resource::root("com.example.notes"),
        )
        .encode_canonical()
        .unwrap()
    }

    fn declared_schema() -> Schema {
        let notes = schema("notes", 1);
        let analyzer = AnalyzerRef {
            schema: notes.clone(),
            name: SchemaId::parse("plain").unwrap(),
        };
        let gate = GateRef {
            schema: notes.clone(),
            name: SchemaId::parse("read").unwrap(),
        };
        Schema {
            reference: notes.clone(),
            sources: vec![SourceRef {
                name: SchemaId::parse("note").unwrap(),
                version: 2,
            }],
            fields: vec![Field {
                reference: field(&notes, "title"),
                kind: FieldKind::Text,
                analyzer: Some(analyzer.clone()),
            }],
            edges: vec![Edge {
                reference: EdgeRef {
                    schema: notes.clone(),
                    name: SchemaId::parse("related").unwrap(),
                },
                target: notes.clone(),
                gate: gate.clone(),
            }],
            gates: vec![Gate {
                reference: gate,
                demand: demand("find.read"),
            }],
            analyzers: vec![Analyzer {
                reference: analyzer,
                configuration: b"unicode-v1".to_vec(),
            }],
            features: vec![Feature {
                reference: FeatureRef {
                    schema: notes,
                    name: SchemaId::parse("semantic").unwrap(),
                },
                stamp: [7; 32],
            }],
            ops: OpSet::ALL,
            modes: ModeSet::ALL,
            bound: bound(5),
        }
    }

    #[test]
    fn descriptor_schema_bytes_and_roundtrip_are_frozen() {
        let schema = declared_schema();
        let bytes = schema.encode().unwrap();
        assert_eq!(Schema::decode_canonical(&bytes).unwrap(), schema);
        assert_eq!(
            bytes,
            vec![
                1, 5, 110, 111, 116, 101, 115, 1, 1, 4, 110, 111, 116, 101, 2, 1, 5, 110, 111, 116,
                101, 115, 1, 5, 116, 105, 116, 108, 101, 4, 1, 5, 110, 111, 116, 101, 115, 1, 5,
                112, 108, 97, 105, 110, 1, 5, 110, 111, 116, 101, 115, 1, 7, 114, 101, 108, 97,
                116, 101, 100, 5, 110, 111, 116, 101, 115, 1, 5, 110, 111, 116, 101, 115, 1, 4,
                114, 101, 97, 100, 1, 5, 110, 111, 116, 101, 115, 1, 4, 114, 101, 97, 100, 32, 1,
                0, 17, 99, 111, 109, 46, 101, 120, 97, 109, 112, 108, 101, 46, 110, 111, 116, 101,
                115, 0, 9, 102, 105, 110, 100, 46, 114, 101, 97, 100, 0, 1, 5, 110, 111, 116, 101,
                115, 1, 5, 112, 108, 97, 105, 110, 10, 117, 110, 105, 99, 111, 100, 101, 45, 118,
                49, 1, 5, 110, 111, 116, 101, 115, 1, 8, 115, 101, 109, 97, 110, 116, 105, 99, 7,
                7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                7, 7, 7, 63, 3, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
            ]
        );
    }

    #[test]
    fn descriptor_schema_coordinates_and_sets_fail_closed() {
        let mut duplicate = declared_schema();
        duplicate.fields.push(duplicate.fields[0].clone());
        assert_eq!(
            duplicate.canonicalized().validate(),
            Err(Invalid::InvalidSet("schema fields"))
        );

        let mut cross_wired = declared_schema();
        cross_wired.fields[0].reference.schema = schema("other", 1);
        assert_eq!(
            cross_wired.validate(),
            Err(Invalid::UndeclaredSchema("declared field"))
        );

        let mut invalid_gate = declared_schema();
        invalid_gate.gates[0].demand = vec![9, 9, 9];
        assert_eq!(
            invalid_gate.validate(),
            Err(Invalid::InvalidOperand("gate demand"))
        );
    }

    #[test]
    fn grant_bytes_and_field_order_are_frozen() {
        let bytes = grant().encode().unwrap();
        assert_eq!(
            bytes,
            vec![
                1, 1, 5, b'n', b'o', b't', b'e', b's', 1, 0x3f, 1, 5, b'n', b'o', b't', b'e', b's',
                1, 5, b't', b'i', b't', b'l', b'e', 0, 0, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            ]
        );
        assert_eq!(Grant::decode_canonical(&bytes).unwrap(), grant());
    }

    #[test]
    fn canonical_sets_bounds_and_schema_containment_fail_closed() {
        let mut invalid = grant();
        invalid.schemas.push(invalid.schemas[0].clone());
        assert_eq!(invalid.validate(), Err(Invalid::InvalidSet("schemas")));

        let mut invalid = grant();
        invalid.ops = OpSet(0x80);
        assert_eq!(invalid.validate(), Err(Invalid::InvalidOps));

        let mut invalid = grant();
        invalid.bound.wall_millis = 0;
        assert_eq!(invalid.validate(), Err(Invalid::InvalidBound));

        let mut invalid = grant();
        invalid.fields[0].schema = schema("other", 1);
        assert_eq!(invalid.validate(), Err(Invalid::UndeclaredSchema("fields")));
    }

    #[test]
    fn grant_composition_can_only_narrow() {
        let parent = grant();
        let mut child = parent.clone();
        child.ops = OpSet::SEEK;
        child.bound = bound(1).intersection(bound(2));
        assert_eq!(child.validate_within(&parent), Ok(()));

        let mut wider = child.clone();
        wider.ops = OpSet::SEEK.union(OpSet::PACK);
        assert_eq!(wider.validate_within(&parent), Ok(()));

        let mut narrow_parent = parent.clone();
        narrow_parent.ops = OpSet::SEEK;
        assert_eq!(
            wider.validate_within(&narrow_parent),
            Err(Invalid::Widening("ops"))
        );

        let mut wider_bound = child;
        wider_bound.bound.wall_millis = 2;
        assert_eq!(
            wider_bound.validate_within(&parent),
            Err(Invalid::Widening("bound"))
        );
    }

    #[test]
    fn grant_cannot_exceed_active_world_declarations() {
        let notes = schema("notes", 1);
        let mut allowed = grant();
        allowed.ops = OpSet::SEEK;
        allowed.edges = vec![EdgeRef {
            schema: notes.clone(),
            name: SchemaId::parse("related").unwrap(),
        }];
        allowed.gates = vec![GateRef {
            schema: notes.clone(),
            name: SchemaId::parse("read").unwrap(),
        }];
        allowed.features = vec![FeatureRef {
            schema: notes,
            name: SchemaId::parse("semantic").unwrap(),
        }];
        assert_eq!(
            allowed.validate_within_schemas(&[declared_schema()]),
            Ok(())
        );

        assert_eq!(
            allowed.validate_within_schemas(&[]),
            Err(Invalid::UndeclaredSchema("grant schema"))
        );

        let mut declaration = declared_schema();
        declaration.ops = OpSet::KEEP;
        assert_eq!(
            allowed.validate_within_schemas(&[declaration]),
            Err(Invalid::NotGranted("operator"))
        );

        let mut declaration = declared_schema();
        declaration.modes = ModeSet::AUGMENTED;
        assert_eq!(
            allowed.validate_within_schemas(&[declaration]),
            Err(Invalid::NotGranted("mode"))
        );

        let mut declaration = declared_schema();
        declaration.bound.wall_millis = 1;
        let mut wider = allowed.clone();
        wider.bound.wall_millis = 2;
        assert_eq!(
            wider.validate_within_schemas(&[declaration]),
            Err(Invalid::NotGranted("bound"))
        );

        let mut declaration = declared_schema();
        declaration.fields.clear();
        assert_eq!(
            allowed.validate_within_schemas(&[declaration]),
            Err(Invalid::NotGranted("field"))
        );

        let mut declaration = declared_schema();
        declaration.edges.clear();
        assert_eq!(
            allowed.validate_within_schemas(&[declaration]),
            Err(Invalid::NotGranted("edge"))
        );

        let mut gate_only = allowed.clone();
        gate_only.edges.clear();
        let mut declaration = declared_schema();
        declaration.edges.clear();
        declaration.gates.clear();
        assert_eq!(
            gate_only.validate_within_schemas(&[declaration]),
            Err(Invalid::NotGranted("gate"))
        );

        let mut declaration = declared_schema();
        declaration.features.clear();
        assert_eq!(
            allowed.validate_within_schemas(&[declaration]),
            Err(Invalid::NotGranted("feature"))
        );
    }

    #[test]
    fn bound_intersection_and_grant_composition_fail_closed_on_every_axis() {
        let left = Bound {
            decoded_bodies: 1,
            postings_read: 20,
            edges_visited: 3,
            nodes_visited: 40,
            paths_retained: 5,
            candidates_per_branch: 60,
            score_evaluations: 7,
            projected_bytes: 80,
            packed_tokens: 9,
            wall_millis: 100,
        };
        let right = Bound {
            decoded_bodies: 10,
            postings_read: 2,
            edges_visited: 30,
            nodes_visited: 4,
            paths_retained: 50,
            candidates_per_branch: 6,
            score_evaluations: 70,
            projected_bytes: 8,
            packed_tokens: 90,
            wall_millis: 10,
        };
        let intersection = left.intersection(right);
        assert_eq!(
            intersection,
            Bound {
                decoded_bodies: 1,
                postings_read: 2,
                edges_visited: 3,
                nodes_visited: 4,
                paths_retained: 5,
                candidates_per_branch: 6,
                score_evaluations: 7,
                projected_bytes: 8,
                packed_tokens: 9,
                wall_millis: 10,
            }
        );
        assert!(left.contains(intersection));
        assert!(right.contains(intersection));

        let parent = grant();
        for axis in 0..10 {
            let mut wider = parent.clone();
            wider.bound = with_bound_axis(wider.bound, axis, 2);
            assert_eq!(
                wider.validate_within(&parent),
                Err(Invalid::Widening("bound")),
                "Bound axis {axis} widened"
            );

            for sentinel in [0, u64::MAX] {
                let mut invalid = parent.clone();
                invalid.bound = with_bound_axis(invalid.bound, axis, sentinel);
                assert_eq!(
                    invalid.validate(),
                    Err(Invalid::InvalidBound),
                    "Bound axis {axis} accepted sentinel {sentinel}"
                );
            }
        }
    }

    #[test]
    fn grant_widening_rejects_every_vocabulary_ceiling() {
        let notes = schema("notes", 1);
        let mut parent = grant();
        parent.ops = OpSet::SEEK;
        parent.edges = vec![EdgeRef {
            schema: notes.clone(),
            name: SchemaId::parse("related").unwrap(),
        }];
        parent.gates = vec![GateRef {
            schema: notes.clone(),
            name: SchemaId::parse("read").unwrap(),
        }];
        parent.features = vec![FeatureRef {
            schema: notes.clone(),
            name: SchemaId::parse("semantic").unwrap(),
        }];

        let mut wider = parent.clone();
        wider.schemas.push(schema("other", 1));
        assert_eq!(
            wider.validate_within(&parent),
            Err(Invalid::Widening("schemas"))
        );

        let mut wider = parent.clone();
        wider.fields.push(field(&notes, "z-field"));
        assert_eq!(
            wider.validate_within(&parent),
            Err(Invalid::Widening("fields"))
        );

        let mut wider = parent.clone();
        wider.edges.push(EdgeRef {
            schema: notes.clone(),
            name: SchemaId::parse("z-edge").unwrap(),
        });
        assert_eq!(
            wider.validate_within(&parent),
            Err(Invalid::Widening("edges"))
        );

        let mut wider = parent.clone();
        wider.gates.push(GateRef {
            schema: notes.clone(),
            name: SchemaId::parse("z-gate").unwrap(),
        });
        assert_eq!(
            wider.validate_within(&parent),
            Err(Invalid::Widening("gates"))
        );

        let mut wider = parent.clone();
        wider.features.push(FeatureRef {
            schema: notes,
            name: SchemaId::parse("z-feature").unwrap(),
        });
        assert_eq!(
            wider.validate_within(&parent),
            Err(Invalid::Widening("features"))
        );

        let mut wider = parent.clone();
        wider.ops = OpSet::SEEK.union(OpSet::PACK);
        assert_eq!(
            wider.validate_within(&parent),
            Err(Invalid::Widening("ops"))
        );

        let mut wider = parent.clone();
        wider.modes = ModeSet::ALL;
        assert_eq!(
            wider.validate_within(&parent),
            Err(Invalid::Widening("modes"))
        );
    }

    #[test]
    fn decoder_rejects_trailing_unknown_and_oversized_bytes() {
        let mut trailing = grant().encode().unwrap();
        trailing.push(0);
        assert_eq!(
            Grant::decode_canonical(&trailing),
            Err(Invalid::NonCanonical)
        );

        let mut unknown_version = grant().encode().unwrap();
        unknown_version[0] = 2;
        assert_eq!(
            Grant::decode_canonical(&unknown_version),
            Err(Invalid::UnsupportedVersion(2))
        );

        assert_eq!(
            Grant::decode_canonical(&vec![0; MAX_GRANT_BYTES.saturating_add(1)]),
            Err(Invalid::TooLarge)
        );
    }

    #[test]
    fn digest_is_domain_separated_and_stable() {
        let digest = grant().digest().unwrap();
        assert_eq!(
            digest.as_bytes(),
            [
                0x15, 0xf4, 0xb9, 0x5a, 0xb1, 0xeb, 0xe3, 0xc0, 0x55, 0xaa, 0xf8, 0x63, 0x21, 0xe7,
                0xd9, 0xce, 0xc7, 0x9f, 0xce, 0xc5, 0xfc, 0x85, 0x4d, 0x4d, 0x4a, 0xc6, 0xf3, 0x75,
                0xf4, 0x61, 0x9b, 0xa2,
            ]
        );
    }

    #[test]
    fn query_bytes_and_digest_are_frozen() {
        let bytes = query().encode().unwrap();
        assert_eq!(
            bytes,
            vec![
                4, 5, b'n', b'o', b't', b'e', b's', 1, 0, 0, 1, 1, 0, 0, 4, 5, b'n', b'o', b't',
                b'e', b's', 1, 5, b't', b'i', b't', b'l', b'e', 1, b'q', 0, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0,
            ]
        );
        assert_eq!(Query::decode_canonical(&bytes).unwrap(), query());
        assert_eq!(
            query().digest().unwrap().as_bytes(),
            [
                231, 150, 217, 33, 98, 111, 3, 139, 137, 206, 47, 196, 150, 194, 212, 147, 237,
                252, 222, 178, 136, 62, 6, 103, 119, 83, 28, 254, 168, 102, 221, 165,
            ]
        );
    }

    #[test]
    fn query_topology_and_type_edges_fail_closed() {
        let mut unstable_output = query();
        unstable_output.steps[0].op = Op::Seek(Seek::Source);
        assert_eq!(unstable_output.validate(), Ok(()));

        let mut invalid = query();
        invalid.steps[0].id = StepId(0);
        assert_eq!(
            invalid.validate(),
            Err(Invalid::InvalidStep(StepId(0), "non-canonical id"))
        );

        let mut unreachable = query();
        unreachable.steps.push(Step {
            id: StepId::new(2).unwrap(),
            input: Vec::new(),
            op: Op::Seek(Seek::Source),
            bound: bound(1),
        });
        assert_eq!(
            unreachable.validate(),
            Err(Invalid::InvalidQuery("unreachable step"))
        );

        let mut duplicate = query();
        duplicate.steps.push(Step {
            id: StepId::new(1).unwrap(),
            input: Vec::new(),
            op: Op::Seek(Seek::Source),
            bound: bound(1),
        });
        assert_eq!(
            duplicate.validate(),
            Err(Invalid::InvalidStep(
                StepId::new(1).unwrap(),
                "non-canonical id"
            ))
        );

        let mut cyclic = query();
        cyclic.steps[0].input = vec![StepId::new(1).unwrap()];
        assert_eq!(
            cyclic.validate(),
            Err(Invalid::InvalidStep(
                StepId::new(1).unwrap(),
                "input is not earlier"
            ))
        );

        let mut unstable = query();
        unstable.steps.push(Step {
            id: StepId::new(2).unwrap(),
            input: Vec::new(),
            op: Op::Seek(Seek::Term {
                field: field(&schema("notes", 1), "title"),
                text: "second".to_owned(),
                kind: Term::Token,
            }),
            bound: bound(1),
        });
        unstable.steps[0].op = Op::Seek(Seek::Term {
            field: field(&schema("notes", 1), "title"),
            text: "first".to_owned(),
            kind: Term::Token,
        });
        unstable.steps.push(Step {
            id: StepId::new(3).unwrap(),
            input: vec![StepId::new(2).unwrap(), StepId::new(1).unwrap()],
            op: Op::Merge(Merge {
                method: MergeMethod::Union,
            }),
            bound: bound(1),
        });
        unstable.output = StepId::new(3).unwrap();
        assert_eq!(unstable.validate(), Err(Invalid::InvalidSet("step inputs")));

        let notes = schema("notes", 1);
        let mut wrong_flow = query();
        wrong_flow.steps.push(Step {
            id: StepId::new(2).unwrap(),
            input: vec![StepId::new(1).unwrap()],
            op: Op::Pack(Pack {
                fields: vec![field(&notes, "title")],
            }),
            bound: bound(1),
        });
        wrong_flow.steps.push(Step {
            id: StepId::new(3).unwrap(),
            input: vec![StepId::new(2).unwrap()],
            op: Op::Walk(Walk {
                edges: vec![EdgeRef {
                    schema: notes.clone(),
                    name: SchemaId::parse("links").unwrap(),
                }],
                direction: Direction::Out,
                min_hops: 1,
                max_hops: 1,
                unique: Unique::Acyclic,
                order: WalkOrder::Breadth,
                emit: Emit::Nodes,
                gate: GateRef {
                    schema: notes,
                    name: SchemaId::parse("read").unwrap(),
                },
            }),
            bound: bound(1),
        });
        wrong_flow.output = StepId::new(3).unwrap();
        assert_eq!(
            wrong_flow.validate(),
            Err(Invalid::InvalidStep(
                StepId::new(3).unwrap(),
                "Walk requires one Nodes input"
            ))
        );
    }

    #[test]
    fn every_operator_flow_edge_is_typed_and_total() {
        let notes = schema("notes", 1);
        let title = field(&notes, "title");
        let seek_source = Op::Seek(Seek::Source);
        let seek_ranked = Op::Seek(Seek::Term {
            field: title.clone(),
            text: "q".to_owned(),
            kind: Term::Token,
        });
        let keep = Op::Keep(Keep {
            predicates: vec![Predicate {
                field: title.clone(),
                test: Test::Equal,
                value: Atom::Text("q".to_owned()),
            }],
        });
        let walk = Op::Walk(Walk {
            edges: vec![EdgeRef {
                schema: notes.clone(),
                name: SchemaId::parse("related").unwrap(),
            }],
            direction: Direction::Out,
            min_hops: 1,
            max_hops: 1,
            unique: Unique::Acyclic,
            order: WalkOrder::Breadth,
            emit: Emit::Paths,
            gate: GateRef {
                schema: notes,
                name: SchemaId::parse("read").unwrap(),
            },
        });
        let rank = Op::Rank(Rank {
            by: vec![RankBy::Field(title.clone())],
        });
        let merge = Op::Merge(Merge {
            method: MergeMethod::Union,
        });
        let pack = Op::Pack(Pack {
            fields: vec![title],
        });

        assert_eq!(seek_source.output(&[]), Ok(Flow::Nodes));
        assert!(seek_source.output(&[Flow::Nodes]).is_err());
        assert_eq!(seek_ranked.output(&[]), Ok(Flow::Ranked));
        assert!(seek_ranked.output(&[Flow::Ranked]).is_err());

        for flow in [Flow::Nodes, Flow::Paths, Flow::Ranked] {
            assert_eq!(keep.output(&[flow]), Ok(flow));
            assert_eq!(rank.output(&[flow]), Ok(Flow::Ranked));
        }
        assert!(keep.output(&[]).is_err());
        assert!(keep.output(&[Flow::Context]).is_err());
        assert!(rank.output(&[Flow::Context]).is_err());

        assert_eq!(walk.output(&[Flow::Nodes]), Ok(Flow::Paths));
        assert!(walk.output(&[Flow::Ranked]).is_err());
        assert_eq!(
            merge.output(&[Flow::Ranked, Flow::Ranked]),
            Ok(Flow::Ranked)
        );
        assert!(merge.output(&[Flow::Ranked]).is_err());
        assert!(merge.output(&[Flow::Ranked, Flow::Nodes]).is_err());
        assert_eq!(pack.output(&[Flow::Ranked]), Ok(Flow::Context));
        assert_eq!(pack.output(&[Flow::Nodes]), Ok(Flow::Context));
    }

    #[test]
    fn cursor_coordinates_and_canonical_envelope_fail_closed() {
        let query = query();
        let coordinates = Coordinates {
            epoch: Epoch::from_u64(7),
            space: SpaceId::parse("ws_00000000000000000000000000").unwrap(),
            world: WorldId::parse("com.example.notes").unwrap(),
            implementation: [3; 32],
            root: [4; 32],
            extractor_schema_digest: crate::publication::ExtractorSchemaDigest::from_digest(
                [9; 32],
            ),
            materialization: crate::publication::MaterializationId::from_u64(11).unwrap(),
            actor: ActorId::from_incept_hash(&"a".repeat(64)),
            device: DeviceId::parse(&"b".repeat(64)).unwrap(),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![5]),
            query: query.digest().unwrap(),
            schema: query.schema.clone(),
        };
        let cursor = Cursor::issue(&coordinates, &query, b"position".to_vec()).unwrap();
        assert_eq!(
            *blake3::hash(cursor.as_bytes()).as_bytes(),
            [
                38, 154, 9, 98, 229, 57, 134, 58, 228, 125, 69, 227, 37, 197, 138, 119, 247, 7,
                129, 238, 173, 35, 82, 29, 9, 14, 169, 235, 7, 131, 69, 206,
            ]
        );
        assert_eq!(Cursor::new(cursor.as_bytes().to_vec()).unwrap(), cursor);
        assert_eq!(cursor.validate_for(&coordinates, &query), Ok(()));

        let mut resumed = query.clone();
        resumed.cursor = Some(cursor.clone());
        assert_eq!(resumed.validate(), Ok(()));
        assert_eq!(
            resumed.cursor_query_digest().unwrap(),
            query.digest().unwrap()
        );

        macro_rules! mismatch {
            ($field:ident, $value:expr) => {{
                let mut hostile = coordinates.clone();
                hostile.$field = $value;
                assert_eq!(
                    cursor.validate_for(&hostile, &query),
                    Err(Invalid::CursorMismatch(stringify!($field)))
                );
            }};
        }
        mismatch!(epoch, Epoch::from_u64(8));
        mismatch!(
            space,
            SpaceId::parse("ws_11111111111111111111111111").unwrap()
        );
        mismatch!(world, WorldId::parse("com.example.other").unwrap());
        mismatch!(implementation, [6; 32]);
        mismatch!(root, [7; 32]);
        mismatch!(
            extractor_schema_digest,
            crate::publication::ExtractorSchemaDigest::from_digest([8; 32])
        );
        mismatch!(
            materialization,
            crate::publication::MaterializationId::from_u64(12).unwrap()
        );
        mismatch!(actor, ActorId::from_incept_hash(&"c".repeat(64)));
        mismatch!(device, DeviceId::parse(&"d".repeat(64)).unwrap());
        mismatch!(
            authority_frontier,
            AuthorityFrontier::from_canonical_bytes(vec![8])
        );
        mismatch!(schema, schema("notes", 2));

        let mut different_query = query.clone();
        let Op::Seek(Seek::Term { text, .. }) = &mut different_query.steps[0].op else {
            panic!("query fixture changed")
        };
        *text = "different".to_owned();
        assert_eq!(
            cursor.validate_for(&coordinates, &different_query),
            Err(Invalid::CursorMismatch("query"))
        );
        let mut different_page = query.clone();
        different_page.page_size = 2;
        different_page.bound = bound(2);
        different_page.steps[0].bound = bound(2);
        assert_eq!(
            cursor.validate_for(&coordinates, &different_page),
            Err(Invalid::CursorMismatch("query"))
        );

        let mut trailing = cursor.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(Cursor::new(trailing), Err(Invalid::InvalidCursor));

        let mut unknown_version = cursor.as_bytes().to_vec();
        unknown_version[0] = 2;
        assert_eq!(Cursor::new(unknown_version), Err(Invalid::InvalidCursor));

        let empty_position = postcard::to_stdvec(&(
            CURSOR_VERSION,
            CursorEnvelope {
                coordinates,
                position: Vec::new(),
            },
        ))
        .unwrap();
        assert_eq!(Cursor::new(empty_position), Err(Invalid::InvalidCursor));
        assert_eq!(
            Cursor::new(vec![0; MAX_CURSOR_BYTES + 1]),
            Err(Invalid::InvalidCursor)
        );
    }

    #[test]
    fn query_bounds_operands_and_grant_containment_fail_closed() {
        let allowed = grant();
        assert_eq!(query().validate_within(&allowed), Ok(()));

        let mut invalid = query();
        invalid.steps[0].bound.wall_millis = 2;
        assert_eq!(
            invalid.validate(),
            Err(Invalid::InvalidStep(
                StepId::new(1).unwrap(),
                "bound exceeds query"
            ))
        );

        let mut invalid = query();
        invalid.cursor = Some(Cursor(Vec::new()));
        assert_eq!(invalid.validate(), Err(Invalid::InvalidCursor));

        let mut invalid = query();
        invalid.page_size = 0;
        assert_eq!(invalid.validate(), Err(Invalid::InvalidQuery("page size")));

        let mut invalid = query();
        invalid.page_size = 2;
        assert_eq!(
            invalid.validate(),
            Err(Invalid::InvalidQuery("page size exceeds candidate bound"))
        );

        let mut invalid = query();
        let Op::Seek(Seek::Term { kind, .. }) = &mut invalid.steps[0].op else {
            panic!("query fixture changed")
        };
        *kind = Term::Phrase;
        assert_eq!(
            invalid.validate(),
            Err(Invalid::InvalidOperand("phrase term unsupported"))
        );

        let mut invalid = query();
        let Op::Seek(Seek::Term { kind, .. }) = &mut invalid.steps[0].op else {
            panic!("query fixture changed")
        };
        *kind = Term::Prefix;
        assert_eq!(
            invalid.validate(),
            Err(Invalid::InvalidOperand("term prefix unsupported"))
        );

        let mut invalid = query();
        invalid.steps[0].bound.wall_millis = 0;
        assert_eq!(
            invalid.validate(),
            Err(Invalid::InvalidStep(
                StepId::new(1).unwrap(),
                "invalid bound"
            ))
        );

        let notes = schema("notes", 1);
        let mut invalid = query();
        invalid.steps.push(Step {
            id: StepId::new(2).unwrap(),
            input: vec![StepId::new(1).unwrap()],
            op: Op::Pack(Pack {
                fields: vec![field(&schema("other", 1), "title")],
            }),
            bound: bound(1),
        });
        invalid.output = StepId::new(2).unwrap();
        assert_eq!(
            invalid.validate(),
            Err(Invalid::UndeclaredSchema("pack field"))
        );

        let mut invalid = query();
        invalid.steps[0].op = Op::Seek(Seek::Feature {
            feature: FeatureRef {
                schema: notes,
                name: SchemaId::parse("embedding").unwrap(),
            },
            probe: vec![1],
        });
        assert_eq!(
            invalid.validate(),
            Err(Invalid::InvalidStep(
                StepId::new(1).unwrap(),
                "feature requires augmented mode"
            ))
        );

        let mut denied = allowed.clone();
        denied.ops = OpSet::PACK;
        assert_eq!(
            query().validate_within(&denied),
            Err(Invalid::NotGranted("operator"))
        );

        let mut denied = allowed;
        denied.modes = ModeSet::AUGMENTED;
        assert_eq!(
            query().validate_within(&denied),
            Err(Invalid::NotGranted("mode"))
        );
    }

    #[test]
    fn query_decoder_rejects_trailing_unknown_and_oversized_bytes() {
        let mut trailing = query().encode().unwrap();
        trailing.push(0);
        assert_eq!(
            Query::decode_canonical(&trailing),
            Err(Invalid::NonCanonical)
        );

        let mut unknown_version = query().encode().unwrap();
        unknown_version[0] = 2;
        assert_eq!(
            Query::decode_canonical(&unknown_version),
            Err(Invalid::UnsupportedVersion(2))
        );

        let mut unknown_mode = query().encode().unwrap();
        unknown_mode[9] = 2;
        assert_eq!(
            Query::decode_canonical(&unknown_mode),
            Err(Invalid::NonCanonical)
        );

        assert_eq!(
            Query::decode_canonical(&vec![0; MAX_QUERY_BYTES.saturating_add(1)]),
            Err(Invalid::TooLarge)
        );
    }
}
