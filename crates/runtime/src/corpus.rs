//! Immutable, structurally shared material for World-defined read indexes.
//!
//! A corpus is derived state. It is never replicated truth and it never grants
//! authority. Its complete owner is a [`WorldPublicationId`], so material from
//! a different Manifest, implementation, extractor contract, or local readable
//! materialization cannot be mixed into a query.
//!
//! Extraction is deliberately outside this module. A trusted World package
//! turns one readable Body image into a [`BodyExtraction`]; this module checks
//! the bounded structural contract and maintains persistent source, node,
//! schema, exact-value, and analyzed-term indexes. Nodes receive compact,
//! generation-local `u32` identities: postings repeat those four-byte
//! identities, while Body-local offset columns and interned graph identities
//! are shared across generations. Replacing one Body path-copies only the
//! persistent-map/vector branches and ordered postings that Body names.

// Corpus is a checked packed-column implementation. All offsets, widths, and
// cardinalities are validated at construction/codec admission before these
// hot paths run; retaining direct indexing and fixed-width arithmetic here is
// what keeps the representation compact enough for the Station memory bound.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::{
    borrow::Borrow,
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    hash::Hash,
    sync::Arc,
};

use imbl::{HashMap as PersistentMap, OrdSet as PersistentSet, Vector as PersistentVector};

use replica::body::BodyKey;
use replica::BodyIx;

use crate::{
    find::{
        BodyExtraction, EdgeRef, ExtractedEdge, ExtractedFeature, ExtractedField, ExtractedNode,
        FeatureRef, FieldRef, NodeKey, SchemaRef, Test, Value,
    },
    publication::WorldPublicationId,
};

/// Bounds applied while accepting extractor output.
///
/// The defaults are generous enough for a large graph node while still making
/// hostile or defective extractor output finite. A host may choose tighter
/// limits, but must use the same limits for a full build and its deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub nodes_per_body: usize,
    pub fields_per_node: usize,
    pub edges_per_node: usize,
    pub features_per_node: usize,
    pub targets_per_edge: usize,
    pub terms_per_field: usize,
    pub value_bytes: usize,
    pub term_bytes: usize,
    pub feature_bytes: usize,
    pub body_stamp_bytes: usize,
    pub retained_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            nodes_per_body: 65_536,
            fields_per_node: 4_096,
            edges_per_node: 4_096,
            features_per_node: 4_096,
            targets_per_edge: 1_048_576,
            terms_per_field: 65_536,
            value_bytes: 1_048_576,
            term_bytes: 65_536,
            feature_bytes: 1_048_576,
            body_stamp_bytes: 4_096,
            retained_bytes: u64::MAX,
        }
    }
}

impl Value {
    fn variable_len(&self) -> usize {
        match self {
            Self::Bytes(value) => value.len(),
            Self::Text(value) => value.len(),
            Self::Bool(_) | Self::Signed(_) | Self::Unsigned(_) => 0,
        }
    }
}

/// A replacement batch explicitly pinned to its old and new publication.
#[derive(Debug, Clone)]
pub(crate) struct CorpusDelta {
    pub base: WorldPublicationId,
    pub next: WorldPublicationId,
    /// Exact immutable Body directory that owns every BodyIx in `next`.
    pub snapshot: Arc<replica::ReadSnapshot>,
    pub bodies: Vec<BodyExtraction>,
}

/// The work charged to one full or incremental build.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildWork {
    pub bodies_replaced: u64,
    pub nodes_removed: u64,
    pub nodes_inserted: u64,
    pub postings_removed: u64,
    pub postings_inserted: u64,
    pub retained_bytes: u64,
}

/// Conservative admission price before a record-shaped corpus is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildMemory {
    /// Bytes retained by the finished immutable Corpus (the snapshot is not
    /// included; callers already pin it independently).
    pub retained_bytes: u64,
    /// Additional temporary headroom while streaming extractor rows into the
    /// persistent columns.
    pub transient_bytes: u64,
}

/// Corpus construction or replacement refused before publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    CoordinateMismatch {
        expected: WorldPublicationId,
        actual: WorldPublicationId,
    },
    DuplicateBody(BodyKey),
    DuplicateNode(NodeKey),
    Limit(&'static str),
    Invalid(&'static str),
}

/// Result of a bounded posting visit. `available` is exact for direct
/// postings. For an ordered sub-range without a count index it is the visited
/// lower bound; callers use the ordered scan API for `has_more` rather than
/// forcing a hidden count traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Visit {
    /// Total matching identities in the immutable posting.
    pub available: usize,
    /// Identities explicitly visited by this call.
    pub visited: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ExactKey {
    field: u32,
    value: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TermKey {
    field: Arc<FieldRef>,
    term: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IncomingEntry {
    edge: Arc<EdgeRef>,
    target: Arc<NodeKey>,
    source: NodeIx,
}

/// A dense identity meaningful only inside one pinned Corpus generation.
/// Public results always materialize the World-owned [`NodeKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct NodeIx(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct VisibilityIx(u32);

/// A persistent sorted directory with dense immutable leaves.
///
/// `imbl`'s HAMT/B+tree maps are excellent general-purpose persistent maps,
/// but a million record Bodies made the per-key node/entry overhead larger
/// than the four-byte generation-local identities stored in them. Directory
/// keys are append/replace-heavy and read-mostly, so 256-entry sorted leaves
/// are the better shape: a replacement copies one bounded leaf plus the
/// persistent vector spine and every other leaf remains shared.
const DIRECTORY_LEAF: usize = 256;

#[derive(Debug, Clone)]
struct DirectoryEntry<K, V> {
    key: K,
    value: V,
}

#[derive(Debug, Clone)]
struct ChunkedDirectory<K, V> {
    leaves: PersistentVector<Arc<[DirectoryEntry<K, V>]>>,
    len: usize,
}

impl<K, V> Default for ChunkedDirectory<K, V> {
    fn default() -> Self {
        Self {
            leaves: PersistentVector::new(),
            len: 0,
        }
    }
}

impl<K: Clone + Ord, V: Clone> ChunkedDirectory<K, V> {
    fn len(&self) -> usize {
        self.len
    }

    fn leaf_for<Q>(&self, key: &Q) -> usize
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut low = 0usize;
        let mut high = self.leaves.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let leaf = self.leaves.get(mid).expect("directory midpoint");
            let last = leaf.last().expect("directory leaves are non-empty");
            if last.key.borrow() < key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low.min(self.leaves.len().saturating_sub(1))
    }

    fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.leaves.is_empty() {
            return None;
        }
        let leaf = self.leaves.get(self.leaf_for(key))?;
        let position = leaf
            .binary_search_by(|entry| entry.key.borrow().cmp(key))
            .ok()?;
        let entry = &leaf[position];
        Some((&entry.key, &entry.value))
    }

    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get_key_value(key).map(|(_, value)| value)
    }

    fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get(key).is_some()
    }

    fn insert(&mut self, key: K, value: V) -> Option<V> {
        if self.leaves.is_empty() {
            self.leaves
                .push_back(Arc::from([DirectoryEntry { key, value }]));
            self.len = 1;
            return None;
        }
        let leaf_index = self.leaf_for(&key);
        let mut leaf = self.leaves[leaf_index].to_vec();
        match leaf.binary_search_by(|entry| entry.key.cmp(&key)) {
            Ok(position) => {
                let old = std::mem::replace(&mut leaf[position].value, value);
                self.leaves.set(leaf_index, Arc::from(leaf));
                Some(old)
            }
            Err(position) => {
                leaf.insert(position, DirectoryEntry { key, value });
                self.len = self.len.saturating_add(1);
                if leaf.len() <= DIRECTORY_LEAF {
                    self.leaves.set(leaf_index, Arc::from(leaf));
                } else {
                    let right = leaf.split_off(leaf.len() / 2);
                    self.leaves.set(leaf_index, Arc::from(leaf));
                    self.leaves.insert(leaf_index + 1, Arc::from(right));
                }
                None
            }
        }
    }

    fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.leaves.is_empty() {
            return None;
        }
        let leaf_index = self.leaf_for(key);
        let mut leaf = self.leaves[leaf_index].to_vec();
        let position = leaf
            .binary_search_by(|entry| entry.key.borrow().cmp(key))
            .ok()?;
        let removed = leaf.remove(position).value;
        self.len = self.len.saturating_sub(1);
        if leaf.is_empty() {
            self.leaves.remove(leaf_index);
        } else {
            self.leaves.set(leaf_index, Arc::from(leaf));
        }
        Some(removed)
    }
}

#[derive(Debug, Clone)]
struct StoredField {
    reference: Arc<FieldRef>,
    value: Arc<Value>,
    gate: Option<Arc<crate::find::GateRef>>,
    terms: InlineRows<Arc<[u8]>>,
}

#[derive(Debug, Clone)]
struct StoredEdge {
    reference: Arc<EdgeRef>,
    gate: Arc<crate::find::GateRef>,
    targets: InlineRows<Arc<NodeKey>>,
}

#[derive(Debug, Clone)]
struct StoredFeature {
    reference: Arc<FeatureRef>,
    gate: Option<Arc<crate::find::GateRef>>,
    value: Arc<[u8]>,
}

#[derive(Debug, Clone)]
struct NodeColumn {
    body: BodyIx,
    key: Arc<NodeKey>,
    gate: Option<Arc<crate::find::GateRef>>,
    fields: InlineRows<StoredField>,
    edges: InlineRows<StoredEdge>,
    features: InlineRows<StoredFeature>,
}

/// Inline the record-shaped singleton case and allocate only true vectors.
/// Issues relation/activity records overwhelmingly have one node, one field
/// group, and one edge group; four Arc slice allocations per record dominated
/// the million-Body publication before this representation.
#[derive(Debug, Clone)]
enum InlineRows<T> {
    None,
    One(T),
    Many(Arc<[T]>),
}

impl<T> InlineRows<T> {
    fn from_vec(mut values: Vec<T>) -> Self {
        match values.len() {
            0 => Self::None,
            1 => Self::One(values.pop().expect("one inline row")),
            _ => Self::Many(Arc::from(values)),
        }
    }
}

impl<T> std::ops::Deref for InlineRows<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::None => &[],
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

#[derive(Debug, Clone)]
enum BodyNodes {
    None,
    One(NodeIx),
    Many(Arc<[NodeIx]>),
}

impl BodyNodes {
    fn from_vec(nodes: Vec<NodeIx>) -> Self {
        match nodes.as_slice() {
            [] => Self::None,
            [node] => Self::One(*node),
            _ => Self::Many(Arc::from(nodes)),
        }
    }
}

impl std::ops::Deref for BodyNodes {
    type Target = [NodeIx];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::None => &[],
            Self::One(node) => std::slice::from_ref(node),
            Self::Many(nodes) => nodes,
        }
    }
}

#[derive(Debug, Clone)]
struct BodyRows {
    nodes: BodyNodes,
    retained_bytes: u64,
}

const PACKED_SEGMENT_BODIES: usize = 4_096;
const PACKED_NONE_U16: u16 = u16::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PackedNodeRef {
    segment: u32,
    node: u32,
}

/// Compact point-lookup key. The canonical bytes remain in the owning
/// segment and are compared after lookup, so the global directory never owns
/// a second `NodeKey` allocation per extracted node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct NodeFingerprint([u8; 16]);

#[derive(Debug, Clone, Copy)]
struct PackedBodyRef {
    segment: u32,
    body: u32,
}

#[derive(Debug, Clone)]
struct PackedBodyRow {
    source: BodyIx,
    node_start: u32,
    node_len: u32,
    retained_bytes: u32,
    posting_count: u32,
}

#[derive(Debug, Clone)]
struct PackedNode {
    key: u32,
    source: BodyIx,
    field_start: u32,
    edge_start: u32,
    feature_start: u32,
    gate: u16,
    field_len: u16,
    edge_len: u16,
    feature_len: u16,
}

#[derive(Debug, Clone)]
struct PackedField {
    value: u32,
    reference: u16,
    gate: u16,
}

#[derive(Debug, Clone)]
struct PackedEdge {
    target_start: u32,
    target_len: u32,
    reference: u16,
    gate: u16,
}

#[derive(Debug, Clone)]
struct PackedFeature {
    reference: u16,
    gate: u16,
    value_start: u32,
    value_len: u32,
}

#[derive(Debug, Clone, Copy)]
struct PackedNodeKey {
    schema: u32,
    start: u32,
    len: u16,
}

/// One scalar dictionary row. Variable-width payloads live in the segment's
/// shared byte slab; fixed values remain inline. No small string/byte value
/// owns an allocator object.
#[derive(Debug, Clone, Copy)]
struct PackedValueMeta(u32);

impl PackedValueMeta {
    fn tag(self) -> u8 {
        (self.0 >> 28) as u8
    }

    fn len(self) -> u32 {
        self.0 & ((1 << 28) - 1)
    }
}

const FRONT_CODED_BLOCK: usize = 16;

/// Sorted byte dictionary with bounded front-coded blocks. Exact lookup binary
/// searches logical rows; decoding one probe inspects at most sixteen entries
/// from a restart. Early lexical inserts rewrite only their immutable segment,
/// never another segment or publication.
#[derive(Debug, Clone, Default)]
struct FrontCodedBytes {
    blocks: Arc<[u32]>,
    bytes: Arc<[u8]>,
    len: u32,
}

impl FrontCodedBytes {
    fn from_values(values: &[Arc<[u8]>]) -> Result<Self, Failure> {
        let len = u32::try_from(values.len()).map_err(|_| Failure::Limit("packed terms"))?;
        let mut blocks = Vec::with_capacity(values.len().div_ceil(FRONT_CODED_BLOCK));
        let mut bytes = Vec::new();
        let mut prior: &[u8] = &[];
        for (index, value) in values.iter().enumerate() {
            if index % FRONT_CODED_BLOCK == 0 {
                blocks.push(
                    u32::try_from(bytes.len())
                        .map_err(|_| Failure::Limit("front-coded term bytes"))?,
                );
                prior = &[];
            }
            let prefix = prior
                .iter()
                .zip(value.iter())
                .take_while(|(left, right)| left == right)
                .count()
                .min(u8::MAX as usize);
            let suffix = &value[prefix..];
            bytes.push(prefix as u8);
            bytes.extend_from_slice(
                &u16::try_from(suffix.len())
                    .map_err(|_| Failure::Limit("front-coded term length"))?
                    .to_be_bytes(),
            );
            bytes.extend_from_slice(suffix);
            prior = value;
        }
        Ok(Self {
            blocks: Arc::from(blocks),
            bytes: Arc::from(bytes),
            len,
        })
    }

    fn len(&self) -> usize {
        self.len as usize
    }

    fn value(&self, index: usize) -> Option<Vec<u8>> {
        if index >= self.len() {
            return None;
        }
        let block = index / FRONT_CODED_BLOCK;
        let first = block * FRONT_CODED_BLOCK;
        let mut at = *self.blocks.get(block)? as usize;
        let mut value = Vec::new();
        for _ in first..=index {
            let prefix = usize::from(*self.bytes.get(at)?);
            let len = u16::from_be_bytes([
                *self.bytes.get(at.saturating_add(1))?,
                *self.bytes.get(at.saturating_add(2))?,
            ]) as usize;
            at = at.saturating_add(3);
            let suffix = self.bytes.get(at..at.saturating_add(len))?;
            if prefix > value.len() {
                return None;
            }
            value.truncate(prefix);
            value.extend_from_slice(suffix);
            at = at.saturating_add(len);
        }
        Some(value)
    }

    fn find(&self, probe: &[u8]) -> Option<u32> {
        let mut low = 0usize;
        let mut high = self.len();
        while low < high {
            let mid = low + (high - low) / 2;
            match self.value(mid)?.as_slice().cmp(probe) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return u32::try_from(mid).ok(),
            }
        }
        None
    }

    fn retained_bytes(&self) -> u64 {
        usize_u64(self.blocks.len())
            .saturating_mul(4)
            .saturating_add(usize_u64(self.bytes.len()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BuildSchemaPosting {
    schema: u16,
    visibility: u16,
    node: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BuildOrderedPosting {
    field: u16,
    visibility: u16,
    value: u32,
    node: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BuildTermPosting {
    field: u16,
    visibility: u16,
    term: u32,
    node: u32,
    frequency: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BuildFeaturePosting {
    feature: u16,
    visibility: u16,
    node: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BuildIncomingPosting {
    edge: u16,
    visibility: u16,
    target: u32,
    node: u32,
}

/// One field/schema/edge/feature and visibility partition in a pair of packed
/// posting streams. Groups are small and binary searched; the millions of
/// repeated keys and visibility ids are not stored once per hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PackedPostingGroup {
    key: u16,
    visibility: u16,
    start: u32,
    len: u32,
}

/// Dense bit-packed u32 column. Posting ids are segment-local, so a stream
/// uses only the bits required by that immutable segment (often 12-19 rather
/// than 32). Access remains O(1), preserving binary seek and heap merge.
#[derive(Debug, Clone, Default)]
struct PackedU32 {
    bytes: Arc<[u8]>,
    len: u32,
    bits: u8,
}

impl PackedU32 {
    fn from_values(values: &[u32]) -> Result<Self, Failure> {
        let len = u32::try_from(values.len()).map_err(|_| Failure::Limit("packed postings"))?;
        let max = values.iter().copied().max().unwrap_or(0);
        let bits = if values.is_empty() {
            0
        } else {
            (u32::BITS - max.leading_zeros()).max(1) as u8
        };
        let total_bits = (values.len() as u64).saturating_mul(u64::from(bits));
        let byte_len = usize::try_from(total_bits.saturating_add(7) / 8)
            .map_err(|_| Failure::Limit("packed posting bytes"))?;
        let mut bytes = vec![0u8; byte_len];
        for (index, value) in values.iter().copied().enumerate() {
            let bit = (index as u64).saturating_mul(u64::from(bits));
            let byte =
                usize::try_from(bit / 8).map_err(|_| Failure::Limit("packed posting offset"))?;
            let shift = (bit % 8) as u32;
            let encoded = u64::from(value) << shift;
            let needed = ((u32::from(bits) + shift).div_ceil(8)) as usize;
            for offset in 0..needed {
                if let Some(slot) = bytes.get_mut(byte.saturating_add(offset)) {
                    *slot |= (encoded >> (offset * 8)) as u8;
                }
            }
        }
        Ok(Self {
            bytes: Arc::from(bytes),
            len,
            bits,
        })
    }

    fn len(&self) -> usize {
        self.len as usize
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn get(&self, index: usize) -> Option<u32> {
        if index >= self.len() || self.bits == 0 {
            return None;
        }
        let bit = (index as u64).saturating_mul(u64::from(self.bits));
        let byte = usize::try_from(bit / 8).ok()?;
        let shift = (bit % 8) as u32;
        let needed = ((u32::from(self.bits) + shift).div_ceil(8)) as usize;
        let mut encoded = 0u64;
        for offset in 0..needed {
            encoded |= u64::from(*self.bytes.get(byte.saturating_add(offset)).unwrap_or(&0))
                << (offset * 8);
        }
        let mask = if self.bits == 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << self.bits) - 1
        };
        Some(((encoded >> shift) & mask) as u32)
    }

    fn partition_point(
        &self,
        start: usize,
        end: usize,
        mut predicate: impl FnMut(u32) -> bool,
    ) -> usize {
        let mut low = start.min(self.len());
        let mut high = end.min(self.len());
        while low < high {
            let mid = low + (high - low) / 2;
            if self.get(mid).is_some_and(&mut predicate) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    fn retained_bytes(&self) -> u64 {
        usize_u64(self.bytes.len())
    }
}

fn pack_node_groups(
    rows: impl IntoIterator<Item = (u16, u16, u32)>,
) -> Result<(Vec<PackedPostingGroup>, PackedU32), Failure> {
    let mut groups = Vec::<PackedPostingGroup>::new();
    let mut nodes = Vec::new();
    for (key, visibility, node) in rows {
        let position = u32::try_from(nodes.len()).map_err(|_| Failure::Limit("packed postings"))?;
        match groups.last_mut() {
            Some(group) if (group.key, group.visibility) == (key, visibility) => {
                group.len = group.len.saturating_add(1);
            }
            _ => groups.push(PackedPostingGroup {
                key,
                visibility,
                start: position,
                len: 1,
            }),
        }
        nodes.push(node);
    }
    Ok((groups, PackedU32::from_values(&nodes)?))
}

fn pack_pair_groups(
    rows: impl IntoIterator<Item = (u16, u16, u32, u32)>,
) -> Result<(Vec<PackedPostingGroup>, PackedU32, PackedU32), Failure> {
    let mut groups = Vec::<PackedPostingGroup>::new();
    let mut primary = Vec::new();
    let mut nodes = Vec::new();
    for (key, visibility, value, node) in rows {
        let position = u32::try_from(nodes.len()).map_err(|_| Failure::Limit("packed postings"))?;
        match groups.last_mut() {
            Some(group) if (group.key, group.visibility) == (key, visibility) => {
                group.len = group.len.saturating_add(1);
            }
            _ => groups.push(PackedPostingGroup {
                key,
                visibility,
                start: position,
                len: 1,
            }),
        }
        primary.push(value);
        nodes.push(node);
    }
    Ok((
        groups,
        PackedU32::from_values(&primary)?,
        PackedU32::from_values(&nodes)?,
    ))
}

fn pack_term_groups(
    rows: impl IntoIterator<Item = (u16, u16, u32, u32, u32)>,
) -> Result<(Vec<PackedPostingGroup>, PackedU32, PackedU32, PackedU32), Failure> {
    let mut groups = Vec::<PackedPostingGroup>::new();
    let mut terms = Vec::new();
    let mut nodes = Vec::new();
    let mut frequencies = Vec::new();
    for (key, visibility, term, node, frequency) in rows {
        let position = u32::try_from(nodes.len()).map_err(|_| Failure::Limit("packed postings"))?;
        match groups.last_mut() {
            Some(group) if (group.key, group.visibility) == (key, visibility) => {
                group.len = group.len.saturating_add(1);
            }
            _ => groups.push(PackedPostingGroup {
                key,
                visibility,
                start: position,
                len: 1,
            }),
        }
        terms.push(term);
        nodes.push(node);
        frequencies.push(frequency);
    }
    Ok((
        groups,
        PackedU32::from_values(&terms)?,
        PackedU32::from_values(&nodes)?,
        PackedU32::from_values(&frequencies)?,
    ))
}

/// One immutable Body-range segment. Every dictionary is sorted, so its u32
/// ids preserve canonical public ordering without repeating Arc pointers in
/// postings. Segment boundaries are stable BodyIx ranges and are also the
/// content-addressed persistence/compaction boundary.
#[derive(Debug, Clone)]
struct PackedSegment {
    body_rows: Arc<[PackedBodyRow]>,
    body_nodes: Arc<[u32]>,
    nodes: Arc<[PackedNode]>,
    fields: Arc<[PackedField]>,
    edges: Arc<[PackedEdge]>,
    features: Arc<[PackedFeature]>,
    feature_bytes: Arc<[u8]>,
    targets: Arc<[u32]>,
    schemas: Arc<[SchemaRef]>,
    field_names: Arc<[FieldRef]>,
    edge_names: Arc<[EdgeRef]>,
    feature_names: Arc<[FeatureRef]>,
    gates: Arc<[crate::find::GateRef]>,
    value_payloads: Arc<[u64]>,
    value_meta: Arc<[PackedValueMeta]>,
    value_bytes: Arc<[u8]>,
    terms: FrontCodedBytes,
    node_keys: Arc<[PackedNodeKey]>,
    node_key_bytes: Arc<[u8]>,
    visibilities: Arc<[Visibility]>,
    schema_groups: Arc<[PackedPostingGroup]>,
    schema_nodes: PackedU32,
    ordered_groups: Arc<[PackedPostingGroup]>,
    ordered_values: PackedU32,
    ordered_nodes: PackedU32,
    term_groups: Arc<[PackedPostingGroup]>,
    term_ids: PackedU32,
    term_nodes: PackedU32,
    term_frequencies: PackedU32,
    feature_groups: Arc<[PackedPostingGroup]>,
    feature_nodes: PackedU32,
    incoming_groups: Arc<[PackedPostingGroup]>,
    incoming_targets: PackedU32,
    incoming_nodes: PackedU32,
    retained_bytes: u64,
    physical_bytes: u64,
    posting_count: u64,
}

#[derive(Debug, Clone, Default)]
struct PackedIndex {
    segments: PersistentVector<Option<Arc<PackedSegment>>>,
    bodies: PersistentVector<Option<PackedBodyRef>>,
    nodes: ChunkedDirectory<NodeFingerprint, PackedNodeRef>,
    /// Bounded delta overlay naming rows shadowed by later segments.
    stale: PersistentSet<PackedNodeRef>,
    body_count: usize,
    node_count: usize,
    retained_bytes: u64,
    physical_bytes: u64,
    posting_count: u64,
}

#[derive(Debug, Clone, Copy)]
struct PackedRange {
    segment: u32,
    position: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
struct PackedFieldRange {
    segment: u32,
    position: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
enum FieldScanBounds<'a> {
    Predicate {
        test: Test,
        value: &'a Value,
    },
    Interval {
        lower: std::ops::Bound<&'a Value>,
        upper: std::ops::Bound<&'a Value>,
    },
}

fn packed_id<T: Ord>(values: &[T], value: &T) -> Result<u32, Failure> {
    let index = values
        .binary_search(value)
        .map_err(|_| Failure::Invalid("missing packed dictionary value"))?;
    u32::try_from(index).map_err(|_| Failure::Limit("packed dictionary rows"))
}

fn packed_small_id<T: Ord>(values: &[T], value: &T) -> Result<u16, Failure> {
    let index = values
        .binary_search(value)
        .map_err(|_| Failure::Invalid("missing packed dictionary value"))?;
    u16::try_from(index).map_err(|_| Failure::Limit("packed small dictionary rows"))
}

fn packed_slice<T>(values: &[T], start: u32, len: u32) -> &[T] {
    let start = start as usize;
    let end = start.saturating_add(len as usize).min(values.len());
    values.get(start..end).unwrap_or(&[])
}

fn value_tag(value: &Value) -> u8 {
    match value {
        Value::Bool(_) => 0,
        Value::Signed(_) => 1,
        Value::Unsigned(_) => 2,
        Value::Bytes(_) => 3,
        Value::Text(_) => 4,
    }
}

fn pack_values(values: &[Value]) -> Result<(Vec<u64>, Vec<PackedValueMeta>, Vec<u8>), Failure> {
    let mut payloads = Vec::with_capacity(values.len());
    let mut meta = Vec::with_capacity(values.len());
    let mut bytes = Vec::new();
    for value in values {
        let (payload, len) = match value {
            Value::Bool(value) => (u64::from(*value), 0),
            Value::Signed(value) => (*value as u64, 0),
            Value::Unsigned(value) => (*value, 0),
            Value::Bytes(value) => {
                let start =
                    u32::try_from(bytes.len()).map_err(|_| Failure::Limit("packed value bytes"))?;
                bytes.extend_from_slice(value);
                let len =
                    u32::try_from(value.len()).map_err(|_| Failure::Limit("packed value bytes"))?;
                (u64::from(start), len)
            }
            Value::Text(value) => {
                let start =
                    u32::try_from(bytes.len()).map_err(|_| Failure::Limit("packed value bytes"))?;
                bytes.extend_from_slice(value.as_bytes());
                let len =
                    u32::try_from(value.len()).map_err(|_| Failure::Limit("packed value bytes"))?;
                (u64::from(start), len)
            }
        };
        if len >= (1 << 28) {
            return Err(Failure::Limit("packed value length"));
        }
        payloads.push(payload);
        meta.push(PackedValueMeta((u32::from(value_tag(value)) << 28) | len));
    }
    Ok((payloads, meta, bytes))
}

fn packed_node_key_id(values: &[NodeKey], value: &NodeKey) -> Result<u32, Failure> {
    let index = values
        .binary_search(value)
        .map_err(|_| Failure::Invalid("missing packed NodeKey"))?;
    u32::try_from(index).map_err(|_| Failure::Limit("packed NodeKey rows"))
}

fn packed_gate_id(
    values: &[crate::find::GateRef],
    value: Option<&crate::find::GateRef>,
) -> Result<u16, Failure> {
    value.map_or(Ok(PACKED_NONE_U16), |value| packed_small_id(values, value))
}

fn node_fingerprint(key: &NodeKey) -> NodeFingerprint {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/corpus/node-fingerprint/1\0");
    hasher.update(&(key.schema.name.as_bytes().len() as u64).to_be_bytes());
    hasher.update(key.schema.name.as_bytes());
    hasher.update(&key.schema.version.to_be_bytes());
    hasher.update(&(key.node.as_bytes().len() as u64).to_be_bytes());
    hasher.update(key.node.as_bytes());
    let mut fingerprint = [0u8; 16];
    fingerprint.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    NodeFingerprint(fingerprint)
}

impl PackedSegment {
    fn build(
        snapshot: &replica::ReadSnapshot,
        mut bodies: Vec<BodyExtraction>,
    ) -> Result<Self, Failure> {
        bodies.sort_by(|left, right| left.body.cmp(&right.body));
        if let Some(duplicate) = bodies.windows(2).find(|pair| pair[0].body == pair[1].body) {
            return Err(Failure::DuplicateBody(duplicate[0].body.clone()));
        }
        for body in &mut bodies {
            canonicalize_extraction(body);
        }

        let mut schemas = BTreeSet::new();
        let mut fields = BTreeSet::new();
        let mut edges = BTreeSet::new();
        let mut features = BTreeSet::new();
        let mut gates = BTreeSet::new();
        let mut values = BTreeSet::new();
        let mut terms = BTreeSet::new();
        let mut node_keys = BTreeSet::new();
        let mut visibilities = BTreeSet::new();
        for body in &bodies {
            for node in &body.nodes {
                schemas.insert(node.key.schema.clone());
                node_keys.insert(node.key.clone());
                if let Some(gate) = &node.gate {
                    gates.insert(gate.clone());
                }
                let node_gate = node.gate.clone().map(Arc::new);
                visibilities.insert(Visibility::node(node_gate.clone()));
                for field in &node.fields {
                    fields.insert(field.reference.clone());
                    values.insert(field.value.clone());
                    if let Some(gate) = &field.gate {
                        gates.insert(gate.clone());
                    }
                    visibilities.insert(Visibility::member(
                        node_gate.clone(),
                        field.gate.clone().map(Arc::new),
                    ));
                    terms.extend(field.terms.iter().cloned());
                }
                for edge in &node.edges {
                    edges.insert(edge.reference.clone());
                    gates.insert(edge.gate.clone());
                    visibilities.insert(Visibility::member(
                        node_gate.clone(),
                        Some(Arc::new(edge.gate.clone())),
                    ));
                    for target in &edge.targets {
                        schemas.insert(target.schema.clone());
                        node_keys.insert(target.clone());
                    }
                }
                for feature in &node.features {
                    features.insert(feature.reference.clone());
                    if let Some(gate) = &feature.gate {
                        gates.insert(gate.clone());
                    }
                    visibilities.insert(Visibility::member(
                        node_gate.clone(),
                        feature.gate.clone().map(Arc::new),
                    ));
                }
            }
        }
        let schemas = schemas.into_iter().collect::<Vec<_>>();
        let field_names = fields.into_iter().collect::<Vec<_>>();
        let edge_names = edges.into_iter().collect::<Vec<_>>();
        let feature_names = features.into_iter().collect::<Vec<_>>();
        let gates = gates.into_iter().collect::<Vec<_>>();
        let values = values.into_iter().collect::<Vec<_>>();
        let terms = terms.into_iter().collect::<Vec<_>>();
        let node_keys = node_keys.into_iter().collect::<Vec<_>>();
        let visibilities = visibilities.into_iter().collect::<Vec<_>>();

        let mut body_meta = Vec::with_capacity(bodies.len());
        let mut source_nodes = Vec::new();
        for body in bodies {
            let Some(source) = snapshot.body_ix(&body.body) else {
                if body.nodes.is_empty() {
                    continue;
                }
                return Err(Failure::Invalid("packed source absent from snapshot"));
            };
            let retained_bytes = body
                .nodes
                .iter()
                .fold(usize_u64(body.stamp.len()), |bytes, node| {
                    bytes.saturating_add(retained_node_bytes(node))
                });
            let posting_count = body.nodes.iter().fold(0u64, |postings, node| {
                postings.saturating_add(crate::find::extracted_postings(node))
            });
            let keys = body
                .nodes
                .iter()
                .map(|node| node.key.clone())
                .collect::<Vec<_>>();
            source_nodes.extend(body.nodes.into_iter().map(|node| (source, node)));
            body_meta.push((source, keys, retained_bytes, posting_count));
        }
        source_nodes.sort_by(|left, right| left.1.key.cmp(&right.1.key));
        let mut packed_nodes = Vec::with_capacity(source_nodes.len());
        let mut packed_fields = Vec::new();
        let mut packed_edges = Vec::new();
        let mut packed_features = Vec::new();
        let mut feature_bytes = Vec::new();
        let mut targets = Vec::new();
        let mut schema_postings = Vec::new();
        let mut ordered_postings = Vec::new();
        let mut term_postings = Vec::new();
        let mut feature_postings = Vec::new();
        let mut incoming_postings = Vec::new();
        for (node_index, (source, node)) in source_nodes.iter().enumerate() {
            let node_index =
                u32::try_from(node_index).map_err(|_| Failure::Limit("packed segment nodes"))?;
            let key = packed_node_key_id(&node_keys, &node.key)?;
            let schema = packed_small_id(&schemas, &node.key.schema)?;
            let node_gate = packed_gate_id(&gates, node.gate.as_ref())?;
            let node_visibility = packed_small_id(
                &visibilities,
                &Visibility::node(node.gate.clone().map(Arc::new)),
            )?;
            schema_postings.push(BuildSchemaPosting {
                schema,
                visibility: node_visibility,
                node: node_index,
            });

            let field_start =
                u32::try_from(packed_fields.len()).map_err(|_| Failure::Limit("packed fields"))?;
            for field in &node.fields {
                let reference = packed_small_id(&field_names, &field.reference)?;
                let value = packed_id(&values, &field.value)?;
                let gate = packed_gate_id(&gates, field.gate.as_ref())?;
                let visibility = packed_small_id(
                    &visibilities,
                    &Visibility::member(
                        node.gate.clone().map(Arc::new),
                        field.gate.clone().map(Arc::new),
                    ),
                )?;
                for term in &field.terms {
                    let term = packed_id(&terms, term)?;
                    term_postings.push(BuildTermPosting {
                        field: reference,
                        term,
                        visibility,
                        node: node_index,
                        frequency: 1,
                    });
                }
                packed_fields.push(PackedField {
                    reference,
                    value,
                    gate,
                });
                ordered_postings.push(BuildOrderedPosting {
                    field: reference,
                    visibility,
                    value,
                    node: node_index,
                });
            }

            let edge_start =
                u32::try_from(packed_edges.len()).map_err(|_| Failure::Limit("packed edges"))?;
            for edge in &node.edges {
                let reference = packed_small_id(&edge_names, &edge.reference)?;
                let gate = packed_small_id(&gates, &edge.gate)?;
                let visibility = packed_small_id(
                    &visibilities,
                    &Visibility::member(
                        node.gate.clone().map(Arc::new),
                        Some(Arc::new(edge.gate.clone())),
                    ),
                )?;
                let target_start =
                    u32::try_from(targets.len()).map_err(|_| Failure::Limit("packed targets"))?;
                for target in &edge.targets {
                    let target = packed_node_key_id(&node_keys, target)?;
                    targets.push(target);
                    incoming_postings.push(BuildIncomingPosting {
                        edge: reference,
                        target,
                        visibility,
                        node: node_index,
                    });
                }
                packed_edges.push(PackedEdge {
                    reference,
                    gate,
                    target_start,
                    target_len: u32::try_from(edge.targets.len())
                        .map_err(|_| Failure::Limit("packed targets"))?,
                });
            }

            let feature_start = u32::try_from(packed_features.len())
                .map_err(|_| Failure::Limit("packed features"))?;
            for feature in &node.features {
                let reference = packed_small_id(&feature_names, &feature.reference)?;
                let gate = packed_gate_id(&gates, feature.gate.as_ref())?;
                let visibility = packed_small_id(
                    &visibilities,
                    &Visibility::member(
                        node.gate.clone().map(Arc::new),
                        feature.gate.clone().map(Arc::new),
                    ),
                )?;
                let value_start = u32::try_from(feature_bytes.len())
                    .map_err(|_| Failure::Limit("packed feature bytes"))?;
                feature_bytes.extend_from_slice(&feature.value);
                packed_features.push(PackedFeature {
                    reference,
                    gate,
                    value_start,
                    value_len: u32::try_from(feature.value.len())
                        .map_err(|_| Failure::Limit("packed feature bytes"))?,
                });
                feature_postings.push(BuildFeaturePosting {
                    feature: reference,
                    visibility,
                    node: node_index,
                });
            }
            packed_nodes.push(PackedNode {
                key,
                source: *source,
                gate: node_gate,
                field_start,
                field_len: u16::try_from(node.fields.len())
                    .map_err(|_| Failure::Limit("packed fields"))?,
                edge_start,
                edge_len: u16::try_from(node.edges.len())
                    .map_err(|_| Failure::Limit("packed edges"))?,
                feature_start,
                feature_len: u16::try_from(node.features.len())
                    .map_err(|_| Failure::Limit("packed features"))?,
            });
        }

        schema_postings.sort();
        ordered_postings.sort();
        term_postings.sort();
        feature_postings.sort();
        incoming_postings.sort();

        let posting_count = schema_postings
            .len()
            .saturating_add(ordered_postings.len().saturating_mul(2))
            .saturating_add(term_postings.len())
            .saturating_add(feature_postings.len())
            .saturating_add(incoming_postings.len());
        let (schema_groups, schema_nodes) = pack_node_groups(
            schema_postings
                .into_iter()
                .map(|row| (row.schema, row.visibility, row.node)),
        )?;
        let (ordered_groups, ordered_values, ordered_nodes) = pack_pair_groups(
            ordered_postings
                .into_iter()
                .map(|row| (row.field, row.visibility, row.value, row.node)),
        )?;
        let (term_groups, term_ids, term_nodes, term_frequencies) = pack_term_groups(
            term_postings
                .into_iter()
                .map(|row| (row.field, row.visibility, row.term, row.node, row.frequency)),
        )?;
        let (feature_groups, feature_nodes) = pack_node_groups(
            feature_postings
                .into_iter()
                .map(|row| (row.feature, row.visibility, row.node)),
        )?;
        let (incoming_groups, incoming_targets, incoming_nodes) = pack_pair_groups(
            incoming_postings
                .into_iter()
                .map(|row| (row.edge, row.visibility, row.target, row.node)),
        )?;

        let mut body_rows = Vec::with_capacity(body_meta.len());
        let mut body_nodes = Vec::new();
        for (source, keys, retained_bytes, posting_count) in body_meta {
            let node_start =
                u32::try_from(body_nodes.len()).map_err(|_| Failure::Limit("packed body nodes"))?;
            for key in keys {
                let node = source_nodes
                    .binary_search_by(|(_, node)| node.key.cmp(&key))
                    .map_err(|_| Failure::Invalid("missing packed source node"))?;
                body_nodes
                    .push(u32::try_from(node).map_err(|_| Failure::Limit("packed body nodes"))?);
            }
            body_rows.push(PackedBodyRow {
                source,
                node_start,
                node_len: u32::try_from(body_nodes.len())
                    .map_err(|_| Failure::Limit("packed body nodes"))?
                    .saturating_sub(node_start),
                retained_bytes: u32::try_from(retained_bytes)
                    .map_err(|_| Failure::Limit("packed Body retained bytes"))?,
                posting_count: u32::try_from(posting_count)
                    .map_err(|_| Failure::Limit("packed Body postings"))?,
            });
        }
        body_rows.sort_by_key(|row| row.source);
        let retained_bytes = body_rows.iter().fold(0u64, |bytes, row| {
            bytes.saturating_add(u64::from(row.retained_bytes))
        });
        let mut node_key_bytes = Vec::new();
        let mut packed_node_keys = Vec::with_capacity(node_keys.len());
        for key in &node_keys {
            let start = u32::try_from(node_key_bytes.len())
                .map_err(|_| Failure::Limit("packed NodeKey bytes"))?;
            node_key_bytes.extend_from_slice(key.node.as_bytes());
            packed_node_keys.push(PackedNodeKey {
                schema: packed_id(&schemas, &key.schema)?,
                start,
                len: u16::try_from(key.node.as_bytes().len())
                    .map_err(|_| Failure::Limit("packed NodeId bytes"))?,
            });
        }
        let value_variable_bytes = values.iter().fold(0u64, |bytes, value| {
            bytes.saturating_add(usize_u64(value.variable_len()))
        });
        let (value_payloads, value_meta, value_bytes) = pack_values(&values)?;
        let packed_terms = FrontCodedBytes::from_values(&terms)?;
        let array_bytes =
            |rows: usize, width: usize| usize_u64(rows).saturating_mul(usize_u64(width));
        let fixed_bytes = array_bytes(body_rows.len(), std::mem::size_of::<PackedBodyRow>())
            .saturating_add(array_bytes(body_nodes.len(), std::mem::size_of::<u32>()))
            .saturating_add(array_bytes(
                packed_nodes.len(),
                std::mem::size_of::<PackedNode>(),
            ))
            .saturating_add(array_bytes(
                packed_fields.len(),
                std::mem::size_of::<PackedField>(),
            ))
            .saturating_add(array_bytes(
                packed_edges.len(),
                std::mem::size_of::<PackedEdge>(),
            ))
            .saturating_add(array_bytes(
                packed_features.len(),
                std::mem::size_of::<PackedFeature>(),
            ))
            .saturating_add(array_bytes(targets.len(), std::mem::size_of::<u32>()))
            .saturating_add(array_bytes(schemas.len(), std::mem::size_of::<SchemaRef>()))
            .saturating_add(array_bytes(
                field_names.len(),
                std::mem::size_of::<FieldRef>(),
            ))
            .saturating_add(array_bytes(
                edge_names.len(),
                std::mem::size_of::<EdgeRef>(),
            ))
            .saturating_add(array_bytes(
                feature_names.len(),
                std::mem::size_of::<FeatureRef>(),
            ))
            .saturating_add(array_bytes(
                gates.len(),
                std::mem::size_of::<crate::find::GateRef>(),
            ))
            .saturating_add(array_bytes(
                value_payloads.len(),
                std::mem::size_of::<u64>(),
            ))
            .saturating_add(array_bytes(
                value_meta.len(),
                std::mem::size_of::<PackedValueMeta>(),
            ))
            .saturating_add(packed_terms.retained_bytes())
            .saturating_add(array_bytes(
                packed_node_keys.len(),
                std::mem::size_of::<PackedNodeKey>(),
            ))
            .saturating_add(usize_u64(node_key_bytes.len()))
            .saturating_add(array_bytes(
                visibilities.len(),
                std::mem::size_of::<Visibility>(),
            ))
            .saturating_add(array_bytes(
                schema_groups.len(),
                std::mem::size_of::<PackedPostingGroup>(),
            ))
            .saturating_add(schema_nodes.retained_bytes())
            .saturating_add(array_bytes(
                ordered_groups.len(),
                std::mem::size_of::<PackedPostingGroup>(),
            ))
            .saturating_add(ordered_values.retained_bytes())
            .saturating_add(ordered_nodes.retained_bytes())
            .saturating_add(array_bytes(
                term_groups.len(),
                std::mem::size_of::<PackedPostingGroup>(),
            ))
            .saturating_add(term_ids.retained_bytes())
            .saturating_add(term_nodes.retained_bytes())
            .saturating_add(term_frequencies.retained_bytes())
            .saturating_add(array_bytes(
                feature_groups.len(),
                std::mem::size_of::<PackedPostingGroup>(),
            ))
            .saturating_add(feature_nodes.retained_bytes())
            .saturating_add(array_bytes(
                incoming_groups.len(),
                std::mem::size_of::<PackedPostingGroup>(),
            ))
            .saturating_add(incoming_targets.retained_bytes())
            .saturating_add(incoming_nodes.retained_bytes());
        let variable_bytes = value_variable_bytes.saturating_add(usize_u64(feature_bytes.len()));
        // Includes Arc headers, allocator size classes, and PersistentVector
        // leaf slack. Release fixtures assert this remains conservative
        // against RSS rather than treating Rust struct size as residency.
        let physical_bytes = fixed_bytes
            .saturating_add(variable_bytes)
            .saturating_mul(2)
            .saturating_add(64 * 1024);
        Ok(Self {
            body_rows: Arc::from(body_rows),
            body_nodes: Arc::from(body_nodes),
            nodes: Arc::from(packed_nodes),
            fields: Arc::from(packed_fields),
            edges: Arc::from(packed_edges),
            features: Arc::from(packed_features),
            feature_bytes: Arc::from(feature_bytes),
            targets: Arc::from(targets),
            schemas: Arc::from(schemas),
            field_names: Arc::from(field_names),
            edge_names: Arc::from(edge_names),
            feature_names: Arc::from(feature_names),
            gates: Arc::from(gates),
            value_payloads: Arc::from(value_payloads),
            value_meta: Arc::from(value_meta),
            value_bytes: Arc::from(value_bytes),
            terms: packed_terms,
            node_keys: Arc::from(packed_node_keys),
            node_key_bytes: Arc::from(node_key_bytes),
            visibilities: Arc::from(visibilities),
            schema_groups: Arc::from(schema_groups),
            schema_nodes,
            ordered_groups: Arc::from(ordered_groups),
            ordered_values,
            ordered_nodes,
            term_groups: Arc::from(term_groups),
            term_ids,
            term_nodes,
            term_frequencies,
            feature_groups: Arc::from(feature_groups),
            feature_nodes,
            incoming_groups: Arc::from(incoming_groups),
            incoming_targets,
            incoming_nodes,
            retained_bytes,
            physical_bytes,
            posting_count: usize_u64(posting_count),
        })
    }

    fn node_key(&self, node: u32) -> Option<NodeKey> {
        let row = self.nodes.get(node as usize)?;
        self.target_key(row.key)
    }

    fn node_fingerprint(&self, node: u32) -> Option<NodeFingerprint> {
        let row = self.nodes.get(node as usize)?;
        let key = self.node_keys.get(row.key as usize)?;
        let schema = self.schemas.get(key.schema as usize)?;
        let bytes = packed_slice(&self.node_key_bytes, key.start, u32::from(key.len));
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lait/corpus/node-fingerprint/1\0");
        hasher.update(&(schema.name.as_bytes().len() as u64).to_be_bytes());
        hasher.update(schema.name.as_bytes());
        hasher.update(&schema.version.to_be_bytes());
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
        let mut fingerprint = [0u8; 16];
        fingerprint.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        Some(NodeFingerprint(fingerprint))
    }

    fn target_key(&self, key: u32) -> Option<NodeKey> {
        let key = self.node_keys.get(key as usize)?;
        Some(NodeKey {
            schema: self.schemas.get(key.schema as usize)?.clone(),
            node: crate::find::NodeId::new(
                packed_slice(&self.node_key_bytes, key.start, u32::from(key.len)).to_vec(),
            )
            .ok()?,
        })
    }

    fn target_key_id(&self, target: &NodeKey) -> Option<u32> {
        let position = self
            .node_keys
            .binary_search_by(|held| {
                let Some(schema) = self.schemas.get(held.schema as usize) else {
                    return std::cmp::Ordering::Less;
                };
                let ordering = schema.cmp(&target.schema);
                if !ordering.is_eq() {
                    return ordering;
                }
                packed_slice(&self.node_key_bytes, held.start, u32::from(held.len))
                    .cmp(target.node.as_bytes())
            })
            .ok()?;
        u32::try_from(position).ok()
    }

    fn value(&self, value: u32) -> Option<Value> {
        let payload = *self.value_payloads.get(value as usize)?;
        let meta = *self.value_meta.get(value as usize)?;
        match meta.tag() {
            0 => Some(Value::Bool(payload != 0)),
            1 => Some(Value::Signed(payload as i64)),
            2 => Some(Value::Unsigned(payload)),
            3 => Some(Value::bytes(
                packed_slice(&self.value_bytes, payload as u32, meta.len()).to_vec(),
            )),
            4 => Some(Value::text(
                std::str::from_utf8(packed_slice(&self.value_bytes, payload as u32, meta.len()))
                    .ok()?
                    .to_owned(),
            )),
            _ => None,
        }
    }

    fn compare_value(&self, held: u32, probe: &Value) -> std::cmp::Ordering {
        let Some(payload) = self.value_payloads.get(held as usize).copied() else {
            return std::cmp::Ordering::Less;
        };
        let Some(meta) = self.value_meta.get(held as usize).copied() else {
            return std::cmp::Ordering::Less;
        };
        let tag = value_tag(probe);
        match meta.tag().cmp(&tag) {
            std::cmp::Ordering::Equal => {}
            ordering => return ordering,
        }
        match probe {
            Value::Bool(probe) => (payload != 0).cmp(probe),
            Value::Signed(probe) => (payload as i64).cmp(probe),
            Value::Unsigned(probe) => payload.cmp(probe),
            Value::Bytes(probe) => {
                packed_slice(&self.value_bytes, payload as u32, meta.len()).cmp(probe)
            }
            Value::Text(probe) => {
                packed_slice(&self.value_bytes, payload as u32, meta.len()).cmp(probe.as_bytes())
            }
        }
    }

    fn term_id(&self, term: &[u8]) -> Option<u32> {
        self.terms.find(term)
    }

    fn visibility_admitted(
        &self,
        visibility: u32,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> bool {
        self.visibilities
            .get(visibility as usize)
            .is_some_and(|visibility| visibility.admitted(allows))
    }
}

impl PackedIndex {
    fn segment(&self, index: u32) -> Option<&Arc<PackedSegment>> {
        self.segments.get(index as usize)?.as_ref()
    }

    fn node_ref(&self, key: &NodeKey) -> Option<PackedNodeRef> {
        let reference = self.nodes.get(&node_fingerprint(key)).copied()?;
        self.segment(reference.segment)
            .and_then(|segment| segment.node_key(reference.node))
            .filter(|held| held == key)
            .map(|_| reference)
    }

    fn is_live(&self, reference: PackedNodeRef) -> bool {
        self.segment(reference.segment)
            .and_then(|segment| segment.node_fingerprint(reference.node))
            .and_then(|key| self.nodes.get(&key))
            .is_some_and(|held| *held == reference)
    }

    fn body_work(&self, source: BodyIx) -> Option<(u64, u64, u64)> {
        let owner = self
            .bodies
            .get(source.as_u32() as usize)
            .and_then(|owner| *owner)?;
        let segment = self.segment(owner.segment)?;
        let body = segment.body_rows.get(owner.body as usize)?;
        Some((
            u64::from(body.node_len),
            u64::from(body.posting_count),
            u64::from(body.retained_bytes),
        ))
    }

    fn remove_body(&mut self, source: BodyIx) -> u64 {
        let Some(owner) = self
            .bodies
            .get(source.as_u32() as usize)
            .and_then(|owner| *owner)
        else {
            return 0;
        };
        let Some(segment) = self.segment(owner.segment).cloned() else {
            return 0;
        };
        let Some(body) = segment.body_rows.get(owner.body as usize) else {
            return 0;
        };
        let start = body.node_start as usize;
        let end = start
            .saturating_add(body.node_len as usize)
            .min(segment.body_nodes.len());
        for node in &segment.body_nodes[start..end] {
            if let Some(key) = segment.node_fingerprint(*node) {
                let reference = PackedNodeRef {
                    segment: owner.segment,
                    node: *node,
                };
                if self.nodes.get(&key).is_some_and(|held| *held == reference)
                    && self.nodes.remove(&key).is_some()
                {
                    self.node_count = self.node_count.saturating_sub(1);
                    self.stale.insert(reference);
                }
            }
        }
        if let Some(slot) = self.bodies.get_mut(source.as_u32() as usize) {
            *slot = None;
        }
        self.body_count = self.body_count.saturating_sub(1);
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(u64::from(body.retained_bytes));
        u64::from(body.retained_bytes)
    }

    fn insert_segment(&mut self, segment: PackedSegment) -> Result<BuildWork, Failure> {
        let segment_id = u32::try_from(self.segments.len())
            .map_err(|_| Failure::Limit("packed Corpus segments"))?;
        let segment = Arc::new(segment);
        for body in segment.body_rows.iter() {
            while self.bodies.len() <= body.source.as_u32() as usize {
                self.bodies.push_back(None);
            }
            if self.bodies[body.source.as_u32() as usize].is_some() {
                return Err(Failure::Invalid("occupied packed Body slot"));
            }
        }
        for (node, _) in segment.nodes.iter().enumerate() {
            let node = u32::try_from(node).map_err(|_| Failure::Limit("packed nodes"))?;
            let key = segment
                .node_key(node)
                .ok_or(Failure::Invalid("packed node key"))?;
            let fingerprint = node_fingerprint(&key);
            if let Some(held) = self.nodes.get(&fingerprint).copied() {
                let held_key = self
                    .segment(held.segment)
                    .and_then(|held_segment| held_segment.node_key(held.node))
                    .ok_or(Failure::Invalid("packed point directory"))?;
                if held_key == key {
                    return Err(Failure::DuplicateNode(key));
                }
                return Err(Failure::Invalid("packed NodeKey fingerprint collision"));
            }
        }
        for (body, row) in segment.body_rows.iter().enumerate() {
            self.bodies.set(
                row.source.as_u32() as usize,
                Some(PackedBodyRef {
                    segment: segment_id,
                    body: u32::try_from(body).map_err(|_| Failure::Limit("packed Body rows"))?,
                }),
            );
        }
        for (node, _) in segment.nodes.iter().enumerate() {
            let node = u32::try_from(node).map_err(|_| Failure::Limit("packed nodes"))?;
            let key = segment
                .node_key(node)
                .ok_or(Failure::Invalid("packed node key"))?;
            self.nodes.insert(
                node_fingerprint(&key),
                PackedNodeRef {
                    segment: segment_id,
                    node,
                },
            );
        }
        let postings = usize::try_from(segment.posting_count).unwrap_or(usize::MAX);
        let work = BuildWork {
            bodies_replaced: usize_u64(segment.body_rows.len()),
            nodes_inserted: usize_u64(segment.nodes.len()),
            postings_inserted: usize_u64(postings),
            retained_bytes: segment.retained_bytes,
            ..BuildWork::default()
        };
        self.body_count = self.body_count.saturating_add(segment.body_rows.len());
        self.node_count = self.node_count.saturating_add(segment.nodes.len());
        self.retained_bytes = self.retained_bytes.saturating_add(segment.retained_bytes);
        self.physical_bytes = self.physical_bytes.saturating_add(segment.physical_bytes);
        self.posting_count = self.posting_count.saturating_add(usize_u64(postings));
        self.segments.push_back(Some(segment));
        Ok(work)
    }

    fn materialize(
        &self,
        reference: PackedNodeRef,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> Option<ExtractedNode> {
        if !self.is_live(reference) {
            return None;
        }
        let segment = self.segment(reference.segment)?;
        let row = segment.nodes.get(reference.node as usize)?;
        let gate = (row.gate != PACKED_NONE_U16)
            .then(|| segment.gates.get(row.gate as usize))
            .flatten();
        if !allows(gate) {
            return None;
        }
        let fields = packed_slice(&segment.fields, row.field_start, u32::from(row.field_len))
            .iter()
            .filter_map(|field| {
                let gate = (field.gate != PACKED_NONE_U16)
                    .then(|| segment.gates.get(field.gate as usize))
                    .flatten();
                if !allows(gate) {
                    return None;
                }
                Some(ExtractedField {
                    reference: segment.field_names.get(field.reference as usize)?.clone(),
                    value: segment.value(field.value)?,
                    gate: gate.cloned(),
                    // Analyzer terms are an index-only channel. Reverse term
                    // materialization duplicated every hit and Pack never
                    // exposes analyzer output.
                    terms: Vec::new(),
                })
            })
            .collect();
        let edges = packed_slice(&segment.edges, row.edge_start, u32::from(row.edge_len))
            .iter()
            .filter_map(|edge| {
                let gate = segment.gates.get(edge.gate as usize)?;
                if !allows(Some(gate)) {
                    return None;
                }
                Some(ExtractedEdge {
                    reference: segment.edge_names.get(edge.reference as usize)?.clone(),
                    gate: gate.clone(),
                    targets: packed_slice(&segment.targets, edge.target_start, edge.target_len)
                        .iter()
                        .filter_map(|target| segment.target_key(*target))
                        .collect(),
                })
            })
            .collect();
        let features = packed_slice(
            &segment.features,
            row.feature_start,
            u32::from(row.feature_len),
        )
        .iter()
        .filter_map(|feature| {
            let gate = (feature.gate != PACKED_NONE_U16)
                .then(|| segment.gates.get(feature.gate as usize))
                .flatten();
            if !allows(gate) {
                return None;
            }
            Some(ExtractedFeature {
                reference: segment
                    .feature_names
                    .get(feature.reference as usize)?
                    .clone(),
                gate: gate.cloned(),
                value: Arc::from(
                    packed_slice(
                        &segment.feature_bytes,
                        feature.value_start,
                        feature.value_len,
                    )
                    .to_vec(),
                ),
            })
        })
        .collect();
        Some(ExtractedNode {
            key: segment.node_key(reference.node)?,
            gate: gate.cloned(),
            fields,
            edges,
            features,
        })
    }

    fn grouped_ranges(
        &self,
        groups: fn(&PackedSegment) -> &[PackedPostingGroup],
        nodes: fn(&PackedSegment) -> &PackedU32,
        resume: Option<&NodeKey>,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut key: impl FnMut(&PackedSegment) -> Option<u16>,
    ) -> Vec<PackedRange> {
        let mut ranges = Vec::new();
        for (segment_id, held) in self.segments.iter().enumerate() {
            let Some(segment) = held.as_ref() else {
                continue;
            };
            let Some(key) = key(segment) else {
                continue;
            };
            for visibility in 0..segment.visibilities.len() {
                let visibility = u32::try_from(visibility).unwrap_or(u32::MAX);
                if !segment.visibility_admitted(visibility, allows) {
                    continue;
                }
                let group_key = (key, u16::try_from(visibility).unwrap_or(u16::MAX));
                let Some(group) = groups(segment)
                    .binary_search_by(|group| (group.key, group.visibility).cmp(&group_key))
                    .ok()
                    .and_then(|index| groups(segment).get(index))
                else {
                    continue;
                };
                let mut start = group.start as usize;
                let end = start
                    .saturating_add(group.len as usize)
                    .min(nodes(segment).len());
                if start >= end {
                    continue;
                }
                if let Some(resume) = resume {
                    start = nodes(segment).partition_point(start, end, |node| {
                        segment.node_key(node).is_some_and(|key| &key < resume)
                    });
                }
                if start < end {
                    ranges.push(PackedRange {
                        segment: u32::try_from(segment_id).unwrap_or(u32::MAX),
                        position: start,
                        end,
                    });
                }
            }
        }
        ranges
    }

    fn term_ranges(
        &self,
        field: &FieldRef,
        term: &[u8],
        resume: Option<&NodeKey>,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> Vec<PackedRange> {
        let mut ranges = Vec::new();
        for (segment_id, held) in self.segments.iter().enumerate() {
            let Some(segment) = held.as_ref() else {
                continue;
            };
            let Ok(field) = segment.field_names.binary_search(field) else {
                continue;
            };
            let Some(term) = segment.term_id(term) else {
                continue;
            };
            let field = u16::try_from(field).unwrap_or(u16::MAX);
            for visibility in 0..segment.visibilities.len() {
                let visibility_u32 = u32::try_from(visibility).unwrap_or(u32::MAX);
                if !segment.visibility_admitted(visibility_u32, allows) {
                    continue;
                }
                let visibility = u16::try_from(visibility).unwrap_or(u16::MAX);
                let group_key = (field, visibility);
                let Some(group) = segment
                    .term_groups
                    .binary_search_by(|group| (group.key, group.visibility).cmp(&group_key))
                    .ok()
                    .and_then(|index| segment.term_groups.get(index))
                else {
                    continue;
                };
                let group_start = group.start as usize;
                let group_end = group_start
                    .saturating_add(group.len as usize)
                    .min(segment.term_ids.len());
                let mut start = segment
                    .term_ids
                    .partition_point(group_start, group_end, |held| held < term);
                let end = segment
                    .term_ids
                    .partition_point(group_start, group_end, |held| held <= term);
                if let Some(resume) = resume {
                    start = segment.term_nodes.partition_point(start, end, |node| {
                        segment.node_key(node).is_some_and(|key| &key < resume)
                    });
                }
                if start < end {
                    ranges.push(PackedRange {
                        segment: u32::try_from(segment_id).unwrap_or(u32::MAX),
                        position: start,
                        end,
                    });
                }
            }
        }
        ranges
    }

    fn pair_ranges(
        &self,
        groups: fn(&PackedSegment) -> &[PackedPostingGroup],
        primary: fn(&PackedSegment) -> &PackedU32,
        nodes: fn(&PackedSegment) -> &PackedU32,
        resume: Option<&NodeKey>,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut lookup: impl FnMut(&PackedSegment) -> Option<(u16, u32)>,
    ) -> Vec<PackedRange> {
        let mut ranges = Vec::new();
        for (segment_id, held) in self.segments.iter().enumerate() {
            let Some(segment) = held.as_ref() else {
                continue;
            };
            let Some((key, value)) = lookup(segment) else {
                continue;
            };
            for visibility in 0..segment.visibilities.len() {
                let visibility_u32 = u32::try_from(visibility).unwrap_or(u32::MAX);
                if !segment.visibility_admitted(visibility_u32, allows) {
                    continue;
                }
                let group_key = (key, u16::try_from(visibility).unwrap_or(u16::MAX));
                let Some(group) = groups(segment)
                    .binary_search_by(|group| (group.key, group.visibility).cmp(&group_key))
                    .ok()
                    .and_then(|index| groups(segment).get(index))
                else {
                    continue;
                };
                let group_start = group.start as usize;
                let group_end = group_start
                    .saturating_add(group.len as usize)
                    .min(primary(segment).len());
                let mut start =
                    primary(segment).partition_point(group_start, group_end, |held| held < value);
                let end =
                    primary(segment).partition_point(group_start, group_end, |held| held <= value);
                if let Some(resume) = resume {
                    start = nodes(segment).partition_point(start, end, |node| {
                        segment.node_key(node).is_some_and(|key| &key < resume)
                    });
                }
                if start < end {
                    ranges.push(PackedRange {
                        segment: u32::try_from(segment_id).unwrap_or(u32::MAX),
                        position: start,
                        end,
                    });
                }
            }
        }
        ranges
    }

    fn count_ranges(
        &self,
        ranges: &[PackedRange],
        nodes: fn(&PackedSegment) -> &PackedU32,
    ) -> usize {
        let total = ranges.iter().fold(0usize, |count, range| {
            count.saturating_add(range.end.saturating_sub(range.position))
        });
        let stale = self.stale.iter().fold(0usize, |count, stale| {
            let Some(segment) = self.segment(stale.segment) else {
                return count;
            };
            ranges
                .iter()
                .filter(|range| range.segment == stale.segment)
                .fold(count, |count, range| {
                    let start = nodes(segment)
                        .partition_point(range.position, range.end, |node| node < stale.node);
                    let end = nodes(segment)
                        .partition_point(range.position, range.end, |node| node <= stale.node);
                    count.saturating_add(end.saturating_sub(start))
                })
        });
        total.saturating_sub(stale)
    }

    /// Merge segment-local canonical NodeKey runs without constructing a
    /// result-sized union. At most one head per admitted segment/visibility
    /// partition is inspected for each returned row.
    fn next_range_head(
        &self,
        range: &mut PackedRange,
        nodes: fn(&PackedSegment) -> &PackedU32,
    ) -> Option<(Arc<NodeKey>, PackedNodeRef, usize)> {
        let segment = self.segment(range.segment)?;
        while range.position < range.end {
            let position = range.position;
            let reference = PackedNodeRef {
                segment: range.segment,
                node: nodes(segment).get(range.position)?,
            };
            range.position = range.position.saturating_add(1);
            if !self.is_live(reference) {
                continue;
            }
            return Some((
                Arc::new(segment.node_key(reference.node)?),
                reference,
                position,
            ));
        }
        None
    }

    fn scan_ranges(
        &self,
        mut ranges: Vec<PackedRange>,
        nodes: fn(&PackedSegment) -> &PackedU32,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        limit: usize,
        mut visit: impl FnMut(NodeKey, BodyIx, &ExtractedNode) -> bool,
    ) -> usize {
        let mut visited = 0usize;
        let mut heap = BinaryHeap::<Reverse<(Arc<NodeKey>, usize, PackedNodeRef)>>::new();
        for range_index in 0..ranges.len() {
            if let Some((key, reference, _)) = self.next_range_head(&mut ranges[range_index], nodes)
            {
                heap.push(Reverse((key, range_index, reference)));
            }
        }
        while let Some(Reverse((key, range_index, reference))) = heap.pop() {
            let Some(segment) = self.segment(reference.segment) else {
                continue;
            };
            let Some(source) = segment
                .nodes
                .get(reference.node as usize)
                .map(|row| row.source)
            else {
                continue;
            };
            let Some(materialized) = self.materialize(reference, allows) else {
                continue;
            };
            visited = visited.saturating_add(1);
            if !visit(key.as_ref().clone(), source, &materialized) || visited == limit {
                break;
            }
            if let Some((key, reference, _)) = self.next_range_head(&mut ranges[range_index], nodes)
            {
                heap.push(Reverse((key, range_index, reference)));
            }
        }
        visited
    }

    fn scan_term_ranges(
        &self,
        mut ranges: Vec<PackedRange>,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        limit: usize,
        mut visit: impl FnMut(NodeKey, BodyIx, &ExtractedNode, u32) -> bool,
    ) -> usize {
        let mut visited = 0usize;
        let mut heap = BinaryHeap::<Reverse<(Arc<NodeKey>, usize, PackedNodeRef, usize)>>::new();
        for range_index in 0..ranges.len() {
            if let Some((key, reference, position)) =
                self.next_range_head(&mut ranges[range_index], |segment| &segment.term_nodes)
            {
                heap.push(Reverse((key, range_index, reference, position)));
            }
        }
        while let Some(Reverse((key, range_index, reference, position))) = heap.pop() {
            let Some(segment) = self.segment(reference.segment) else {
                continue;
            };
            let Some(source) = segment
                .nodes
                .get(reference.node as usize)
                .map(|row| row.source)
            else {
                continue;
            };
            let Some(materialized) = self.materialize(reference, allows) else {
                continue;
            };
            let frequency = segment.term_frequencies.get(position).unwrap_or(1);
            visited = visited.saturating_add(1);
            if !visit(key.as_ref().clone(), source, &materialized, frequency) || visited == limit {
                break;
            }
            if let Some((key, reference, position)) =
                self.next_range_head(&mut ranges[range_index], |segment| &segment.term_nodes)
            {
                heap.push(Reverse((key, range_index, reference, position)));
            }
        }
        visited
    }

    fn field_ranges(
        &self,
        field: &FieldRef,
        bounds: FieldScanBounds<'_>,
        resume: Option<&(Value, NodeKey)>,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> Vec<PackedFieldRange> {
        let mut ranges = Vec::new();
        for (segment_id, held) in self.segments.iter().enumerate() {
            let Some(segment) = held.as_ref() else {
                continue;
            };
            let Ok(field) = segment.field_names.binary_search(field) else {
                continue;
            };
            let field = u16::try_from(field).unwrap_or(u16::MAX);
            let value_lower = |probe: &Value| {
                let mut low = 0usize;
                let mut high = segment.value_payloads.len();
                while low < high {
                    let mid = low + (high - low) / 2;
                    if segment.compare_value(mid as u32, probe).is_lt() {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                u32::try_from(low).unwrap_or(u32::MAX)
            };
            let value_upper = |probe: &Value| {
                let mut low = 0usize;
                let mut high = segment.value_payloads.len();
                while low < high {
                    let mid = low + (high - low) / 2;
                    if !segment.compare_value(mid as u32, probe).is_gt() {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                u32::try_from(low).unwrap_or(u32::MAX)
            };
            let (value_lo, value_hi) = match bounds {
                FieldScanBounds::Predicate { test, value } => {
                    let (kind_start, kind_end) = value_kind_bounds(value);
                    let kind_lo = value_lower(&kind_start);
                    let kind_hi = kind_end.as_ref().map_or(u32::MAX, value_lower);
                    match test {
                        Test::Equal => (value_lower(value), value_upper(value)),
                        Test::Less => (kind_lo, value_lower(value)),
                        Test::LessOrEqual => (kind_lo, value_upper(value)),
                        Test::Greater => (value_upper(value), kind_hi),
                        Test::GreaterOrEqual => (value_lower(value), kind_hi),
                        Test::Contains => (kind_lo, kind_hi),
                        Test::Prefix => (
                            value_lower(value),
                            next_prefix(value).as_ref().map_or(kind_hi, value_lower),
                        ),
                    }
                }
                FieldScanBounds::Interval { lower, upper } => {
                    use std::ops::Bound::{Excluded, Included, Unbounded};
                    let lower = match lower {
                        Included(value) => value_lower(value),
                        Excluded(value) => value_upper(value),
                        Unbounded => 0,
                    };
                    let upper = match upper {
                        Included(value) => value_upper(value),
                        Excluded(value) => value_lower(value),
                        Unbounded => u32::MAX,
                    };
                    (lower, upper)
                }
            };
            if value_lo >= value_hi {
                continue;
            }
            for visibility in 0..segment.visibilities.len() {
                let visibility_u32 = u32::try_from(visibility).unwrap_or(u32::MAX);
                if !segment.visibility_admitted(visibility_u32, allows) {
                    continue;
                }
                let visibility = u16::try_from(visibility).unwrap_or(u16::MAX);
                let key = (field, visibility);
                let Some(group) = segment
                    .ordered_groups
                    .binary_search_by(|group| (group.key, group.visibility).cmp(&key))
                    .ok()
                    .and_then(|index| segment.ordered_groups.get(index))
                else {
                    continue;
                };
                let group_start = group.start as usize;
                let group_end = group_start
                    .saturating_add(group.len as usize)
                    .min(segment.ordered_values.len());
                let mut start =
                    segment
                        .ordered_values
                        .partition_point(group_start, group_end, |held| held < value_lo);
                let end = segment
                    .ordered_values
                    .partition_point(group_start, group_end, |held| held < value_hi);
                if let Some((resume_value, resume_key)) = resume {
                    let mut low = start;
                    let mut high = end;
                    while low < high {
                        let mid = low + (high - low) / 2;
                        let Some(value) = segment.ordered_values.get(mid) else {
                            break;
                        };
                        let before = match segment.compare_value(value, resume_value) {
                            std::cmp::Ordering::Less => true,
                            std::cmp::Ordering::Greater => false,
                            std::cmp::Ordering::Equal => segment
                                .ordered_nodes
                                .get(mid)
                                .and_then(|node| segment.node_key(node))
                                .is_some_and(|key| &key < resume_key),
                        };
                        if before {
                            low = mid + 1;
                        } else {
                            high = mid;
                        }
                    }
                    start = low;
                }
                if start < end {
                    ranges.push(PackedFieldRange {
                        segment: u32::try_from(segment_id).unwrap_or(u32::MAX),
                        position: start,
                        end,
                    });
                }
            }
        }
        ranges
    }

    fn count_field_ranges(&self, ranges: &[PackedFieldRange]) -> usize {
        ranges.iter().fold(0usize, |count, range| {
            let Some(segment) = self.segment(range.segment) else {
                return count;
            };
            count.saturating_add(
                (range.position..range.end)
                    .filter(|position| {
                        self.is_live(PackedNodeRef {
                            segment: range.segment,
                            node: segment.ordered_nodes.get(*position).unwrap_or(u32::MAX),
                        })
                    })
                    .count(),
            )
        })
    }

    fn count_exact_field_ranges(&self, ranges: &[PackedFieldRange]) -> usize {
        let total = ranges.iter().fold(0usize, |count, range| {
            count.saturating_add(range.end.saturating_sub(range.position))
        });
        let stale = self.stale.iter().fold(0usize, |count, stale| {
            let Some(segment) = self.segment(stale.segment) else {
                return count;
            };
            ranges
                .iter()
                .filter(|range| range.segment == stale.segment)
                .fold(count, |count, range| {
                    let start =
                        segment
                            .ordered_nodes
                            .partition_point(range.position, range.end, |node| node < stale.node);
                    let end =
                        segment
                            .ordered_nodes
                            .partition_point(range.position, range.end, |node| node <= stale.node);
                    count.saturating_add(end.saturating_sub(start))
                })
        });
        total.saturating_sub(stale)
    }

    fn scan_field_ranges(
        &self,
        mut ranges: Vec<PackedFieldRange>,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        limit: usize,
        mut visit: impl FnMut(Value, NodeKey, BodyIx, &ExtractedNode) -> bool,
    ) -> usize {
        let mut visited = 0usize;
        let mut heap = BinaryHeap::<Reverse<(Value, Arc<NodeKey>, usize, PackedNodeRef)>>::new();
        for range_index in 0..ranges.len() {
            if let Some((value, key, reference)) =
                self.next_field_range_head(&mut ranges[range_index])
            {
                heap.push(Reverse((value, key, range_index, reference)));
            }
        }
        while let Some(Reverse((value, key, range_index, reference))) = heap.pop() {
            let Some(segment) = self.segment(reference.segment) else {
                continue;
            };
            let Some(source) = segment
                .nodes
                .get(reference.node as usize)
                .map(|row| row.source)
            else {
                continue;
            };
            let Some(materialized) = self.materialize(reference, allows) else {
                continue;
            };
            visited = visited.saturating_add(1);
            if !visit(value, key.as_ref().clone(), source, &materialized) || visited == limit {
                break;
            }
            if let Some((value, key, reference)) =
                self.next_field_range_head(&mut ranges[range_index])
            {
                heap.push(Reverse((value, key, range_index, reference)));
            }
        }
        visited
    }

    fn next_field_range_head(
        &self,
        range: &mut PackedFieldRange,
    ) -> Option<(Value, Arc<NodeKey>, PackedNodeRef)> {
        let segment = self.segment(range.segment)?;
        while range.position < range.end {
            let value = segment.ordered_values.get(range.position)?;
            let node = segment.ordered_nodes.get(range.position)?;
            range.position = range.position.saturating_add(1);
            let reference = PackedNodeRef {
                segment: range.segment,
                node,
            };
            if !self.is_live(reference) {
                continue;
            }
            return Some((
                segment.value(value)?,
                Arc::new(segment.node_key(node)?),
                reference,
            ));
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OrderedNode {
    key: Arc<NodeKey>,
    node: NodeIx,
}

const POSTING_LEAF: usize = 256;

/// Persistent ordered posting with compact structurally shared leaves.
///
/// General-purpose persistent tree nodes cost more than the four-byte NodeIx
/// payload in the million-record case. A posting edit copies one bounded leaf
/// and the persistent vector spine; range reads binary-seek the first leaf and
/// then walk only explicitly visited entries.
#[derive(Debug, Clone)]
struct ChunkedSet<T> {
    leaves: PersistentVector<Arc<[T]>>,
    len: usize,
}

impl<T> Default for ChunkedSet<T> {
    fn default() -> Self {
        Self {
            leaves: PersistentVector::new(),
            len: 0,
        }
    }
}

impl<T: Clone + Ord> ChunkedSet<T> {
    fn leaf_for(&self, entry: &T) -> usize {
        let mut low = 0usize;
        let mut high = self.leaves.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let leaf = self.leaves.get(mid).expect("posting midpoint");
            if leaf.last().expect("posting leaf").cmp(entry).is_lt() {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low.min(self.leaves.len().saturating_sub(1))
    }

    fn insert(&mut self, entry: T) -> Option<T> {
        if self.leaves.is_empty() {
            self.leaves.push_back(Arc::from([entry]));
            self.len = 1;
            return None;
        }
        let leaf_index = self.leaf_for(&entry);
        let mut leaf = self.leaves[leaf_index].to_vec();
        match leaf.binary_search(&entry) {
            Ok(position) => Some(std::mem::replace(&mut leaf[position], entry)),
            Err(position) => {
                leaf.insert(position, entry);
                self.len = self.len.saturating_add(1);
                if leaf.len() <= POSTING_LEAF {
                    self.leaves.set(leaf_index, Arc::from(leaf));
                } else {
                    let right = leaf.split_off(leaf.len() / 2);
                    self.leaves.set(leaf_index, Arc::from(leaf));
                    self.leaves.insert(leaf_index + 1, Arc::from(right));
                }
                return None;
            }
        }
        .map(|old| {
            self.leaves.set(leaf_index, Arc::from(leaf));
            old
        })
    }

    fn remove(&mut self, entry: &T) -> Option<T> {
        if self.leaves.is_empty() {
            return None;
        }
        let leaf_index = self.leaf_for(entry);
        let mut leaf = self.leaves[leaf_index].to_vec();
        let position = leaf.binary_search(entry).ok()?;
        let removed = leaf.remove(position);
        self.len = self.len.saturating_sub(1);
        if leaf.is_empty() {
            self.leaves.remove(leaf_index);
        } else {
            self.leaves.set(leaf_index, Arc::from(leaf));
        }
        Some(removed)
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn range(&self, bounds: (std::ops::Bound<T>, std::ops::Bound<T>)) -> ChunkedRange<'_, T> {
        use std::ops::Bound;
        if self.leaves.is_empty() {
            return ChunkedRange {
                posting: self,
                leaf: 0,
                row: 0,
                end: bounds.1,
                done: true,
            };
        }
        let (leaf, row) = match &bounds.0 {
            Bound::Unbounded => (0, 0),
            Bound::Included(start) | Bound::Excluded(start) => {
                let leaf = self.leaf_for(start);
                let row = match &bounds.0 {
                    Bound::Included(_) => self.leaves[leaf].partition_point(|entry| entry < start),
                    Bound::Excluded(_) => self.leaves[leaf].partition_point(|entry| entry <= start),
                    Bound::Unbounded => 0,
                };
                (leaf, row)
            }
        };
        ChunkedRange {
            posting: self,
            leaf,
            row,
            end: bounds.1,
            done: false,
        }
    }
}

struct ChunkedRange<'a, T> {
    posting: &'a ChunkedSet<T>,
    leaf: usize,
    row: usize,
    end: std::ops::Bound<T>,
    done: bool,
}

impl<'a, T: Ord> Iterator for ChunkedRange<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        use std::ops::Bound;
        while !self.done && self.leaf < self.posting.leaves.len() {
            let leaf = &self.posting.leaves[self.leaf];
            if self.row >= leaf.len() {
                self.leaf = self.leaf.saturating_add(1);
                self.row = 0;
                continue;
            }
            let entry = &leaf[self.row];
            let admitted = match &self.end {
                Bound::Unbounded => true,
                Bound::Included(end) => entry <= end,
                Bound::Excluded(end) => entry < end,
            };
            if !admitted {
                self.done = true;
                return None;
            }
            self.row = self.row.saturating_add(1);
            return Some(entry);
        }
        None
    }
}

/// The authority dimensions which must both be admitted before an index row
/// exists for one evaluator. Keeping these as posting partitions—not tags on
/// individual entries—means denied populations are never walked, metered, or
/// chosen as cursor look-ahead.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Visibility {
    node: Option<Arc<crate::find::GateRef>>,
    member: Option<Arc<crate::find::GateRef>>,
}

impl Visibility {
    fn node(node: Option<Arc<crate::find::GateRef>>) -> Self {
        Self { node, member: None }
    }

    fn member(
        node: Option<Arc<crate::find::GateRef>>,
        member: Option<Arc<crate::find::GateRef>>,
    ) -> Self {
        Self { node, member }
    }

    fn admitted(&self, allows: &impl Fn(Option<&crate::find::GateRef>) -> bool) -> bool {
        allows(self.node.as_deref()) && allows(self.member.as_deref())
    }
}

/// Inline the overwhelmingly common single visibility partition. A nested
/// HAMT per posting consumed more memory than the compact NodeIx rows it was
/// meant to protect; additional gate combinations pay one small Vec only when
/// they actually exist.
#[derive(Debug, Clone)]
struct Partitioned<T: Ord> {
    first: Option<(Visibility, ChunkedSet<T>)>,
    more: Vec<(Visibility, ChunkedSet<T>)>,
}

impl<T: Ord> Default for Partitioned<T> {
    fn default() -> Self {
        Self {
            first: None,
            more: Vec::new(),
        }
    }
}

impl<T: Clone + Ord> Partitioned<T> {
    fn iter(&self) -> impl Iterator<Item = (&Visibility, &ChunkedSet<T>)> {
        self.first
            .iter()
            .map(|(visibility, posting)| (visibility, posting))
            .chain(
                self.more
                    .iter()
                    .map(|(visibility, posting)| (visibility, posting)),
            )
    }

    fn posting_mut(&mut self, visibility: Visibility) -> &mut ChunkedSet<T> {
        if self.first.is_none() {
            self.first = Some((visibility, ChunkedSet::default()));
            return &mut self.first.as_mut().expect("inserted partition").1;
        }
        if self
            .first
            .as_ref()
            .is_some_and(|(held, _)| held == &visibility)
        {
            return &mut self.first.as_mut().expect("present partition").1;
        }
        if let Some(position) = self.more.iter().position(|(held, _)| held == &visibility) {
            return &mut self.more[position].1;
        }
        self.more.push((visibility, ChunkedSet::default()));
        &mut self.more.last_mut().expect("inserted partition").1
    }

    fn remove(&mut self, visibility: &Visibility, entry: &T) -> bool {
        if self
            .first
            .as_ref()
            .is_some_and(|(held, _)| held == visibility)
        {
            let removed = self
                .first
                .as_mut()
                .is_some_and(|(_, posting)| posting.remove(entry).is_some());
            if self
                .first
                .as_ref()
                .is_some_and(|(_, posting)| posting.is_empty())
            {
                self.first = self.more.pop();
            }
            return removed;
        }
        let Some(position) = self.more.iter().position(|(held, _)| held == visibility) else {
            return false;
        };
        let removed = self.more[position].1.remove(entry).is_some();
        if self.more[position].1.is_empty() {
            self.more.swap_remove(position);
        }
        removed
    }

    fn is_empty(&self) -> bool {
        self.first.is_none()
    }
}

type PartitionedPosting = Partitioned<NodeIx>;
type PartitionedOrderedNodes = Partitioned<OrderedNode>;

const FLAT_COUNT_PROMOTION: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FlatExactEntry {
    key: ExactKey,
    visibility: VisibilityIx,
    node: NodeIx,
}

/// One globally sorted exact-value column.
///
/// High-cardinality fields (Body id, semantic id, source id) otherwise create
/// one HAMT entry plus one posting container per record. This shape stores one
/// primitive row in dense 256-entry leaves and binary-seeks `(field,value)`.
/// Only genuinely shared values (>64 matches) acquire count metadata, keeping
/// exact status/progress counts O(visible partitions) without paying a map
/// allocation for every singleton id.
#[derive(Debug, Clone, Default)]
struct FlatExactIndex {
    rows: ChunkedSet<FlatExactEntry>,
    visibilities: PersistentMap<VisibilityIx, u32>,
    promoted_counts: PersistentMap<ExactKey, Vec<(VisibilityIx, u32)>>,
}

impl FlatExactIndex {
    fn range_for(
        &self,
        key: &ExactKey,
        visibility: &VisibilityIx,
    ) -> ChunkedRange<'_, FlatExactEntry> {
        self.rows.range((
            std::ops::Bound::Included(FlatExactEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node: NodeIx(0),
            }),
            std::ops::Bound::Included(FlatExactEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node: NodeIx(u32::MAX),
            }),
        ))
    }

    fn raw_counts(&self, key: &ExactKey, stop_after: usize) -> Vec<(VisibilityIx, u32)> {
        let mut counts = Vec::new();
        let mut seen = 0usize;
        for (visibility, _) in self.visibilities.iter() {
            let mut count = 0u32;
            for _ in self.range_for(key, visibility) {
                count = count.saturating_add(1);
                seen = seen.saturating_add(1);
                if seen > stop_after {
                    break;
                }
            }
            if count != 0 {
                counts.push((visibility.clone(), count));
            }
            if seen > stop_after {
                break;
            }
        }
        counts
    }

    fn insert(
        &mut self,
        key: ExactKey,
        visibility: VisibilityIx,
        node: NodeIx,
        work: &mut BuildWork,
    ) {
        if self
            .rows
            .insert(FlatExactEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node,
            })
            .is_some()
        {
            return;
        }
        *self.visibilities.entry(visibility.clone()).or_default() = self
            .visibilities
            .get(&visibility)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if let Some(counts) = self.promoted_counts.get_mut(&key) {
            if let Some((_, count)) = counts.iter_mut().find(|(held, _)| held == &visibility) {
                *count = count.saturating_add(1);
            } else {
                counts.push((visibility, 1));
                counts.sort_by(|left, right| left.0.cmp(&right.0));
            }
        } else {
            let counts = self.raw_counts(&key, FLAT_COUNT_PROMOTION);
            if counts
                .iter()
                .map(|(_, count)| usize::try_from(*count).unwrap_or(usize::MAX))
                .sum::<usize>()
                > FLAT_COUNT_PROMOTION
            {
                self.promoted_counts.insert(key, counts);
            }
        }
        work.postings_inserted = work.postings_inserted.saturating_add(1);
    }

    fn remove(
        &mut self,
        key: &ExactKey,
        visibility: &VisibilityIx,
        node: NodeIx,
        work: &mut BuildWork,
    ) {
        if self
            .rows
            .remove(&FlatExactEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node,
            })
            .is_none()
        {
            return;
        }
        if let Some(counts) = self.promoted_counts.get_mut(key) {
            if let Some(position) = counts.iter().position(|(held, _)| held == visibility) {
                counts[position].1 = counts[position].1.saturating_sub(1);
                if counts[position].1 == 0 {
                    counts.remove(position);
                }
            }
            if counts.is_empty() {
                self.promoted_counts.remove(key);
            }
        }
        if let Some(count) = self.visibilities.get(visibility).copied() {
            if count <= 1 {
                self.visibilities.remove(visibility);
            } else {
                self.visibilities.insert(visibility.clone(), count - 1);
            }
        }
        work.postings_removed = work.postings_removed.saturating_add(1);
    }

    fn count(&self, key: &ExactKey, admitted: &impl Fn(VisibilityIx) -> bool) -> usize {
        if let Some(counts) = self.promoted_counts.get(key) {
            return counts
                .iter()
                .filter(|(visibility, _)| admitted(*visibility))
                .map(|(_, count)| usize::try_from(*count).unwrap_or(usize::MAX))
                .sum();
        }
        self.visibilities
            .iter()
            .filter(|(visibility, _)| admitted(**visibility))
            .map(|(visibility, _)| self.range_for(key, visibility).count())
            .sum()
    }

    fn visit(
        &self,
        key: &ExactKey,
        limit: usize,
        admitted: &impl Fn(VisibilityIx) -> bool,
        mut visit: impl FnMut(NodeIx) -> bool,
    ) -> usize {
        let available = self.count(key, admitted);
        let mut iterators = self
            .visibilities
            .iter()
            .filter(|(visibility, _)| admitted(**visibility))
            .map(|(visibility, _)| self.range_for(key, visibility))
            .collect::<Vec<_>>();
        let mut pending = BinaryHeap::new();
        for (partition, iterator) in iterators.iter_mut().enumerate() {
            if let Some(entry) = iterator.next() {
                pending.push(Reverse((entry.node, partition)));
            }
        }
        let mut visited = 0usize;
        while visited < limit {
            let Some(Reverse((node, partition))) = pending.pop() else {
                break;
            };
            visited = visited.saturating_add(1);
            if !visit(node) {
                break;
            }
            if let Some(entry) = iterators[partition].next() {
                pending.push(Reverse((entry.node, partition)));
            }
        }
        available
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FlatTermEntry {
    key: TermKey,
    visibility: VisibilityIx,
    node_key: Arc<NodeKey>,
    node: NodeIx,
}

#[derive(Debug, Clone, Default)]
struct FlatTermIndex {
    rows: ChunkedSet<FlatTermEntry>,
    visibilities: PersistentMap<VisibilityIx, u32>,
    promoted_counts: PersistentMap<TermKey, Vec<(VisibilityIx, u32)>>,
}

fn term_sentinel(key: &TermKey, high: bool) -> Arc<NodeKey> {
    Arc::new(NodeKey {
        schema: key.field.schema.clone(),
        node: crate::find::NodeId::new(if high {
            vec![u8::MAX; crate::find::MAX_NODE_ID_BYTES]
        } else {
            vec![0]
        })
        .expect("canonical term sentinel"),
    })
}

impl FlatTermIndex {
    fn range_for(
        &self,
        key: &TermKey,
        visibility: &VisibilityIx,
        resume: Option<&NodeKey>,
    ) -> ChunkedRange<'_, FlatTermEntry> {
        self.rows.range((
            std::ops::Bound::Included(FlatTermEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node_key: resume
                    .cloned()
                    .map(Arc::new)
                    .unwrap_or_else(|| term_sentinel(key, false)),
                node: NodeIx(0),
            }),
            std::ops::Bound::Included(FlatTermEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node_key: term_sentinel(key, true),
                node: NodeIx(u32::MAX),
            }),
        ))
    }

    fn raw_counts(&self, key: &TermKey, stop_after: usize) -> Vec<(VisibilityIx, u32)> {
        let mut counts = Vec::new();
        let mut seen = 0usize;
        for (visibility, _) in self.visibilities.iter() {
            let mut count = 0u32;
            for _ in self.range_for(key, visibility, None) {
                count = count.saturating_add(1);
                seen = seen.saturating_add(1);
                if seen > stop_after {
                    break;
                }
            }
            if count != 0 {
                counts.push((visibility.clone(), count));
            }
            if seen > stop_after {
                break;
            }
        }
        counts
    }

    fn insert(
        &mut self,
        key: TermKey,
        visibility: VisibilityIx,
        node_key: Arc<NodeKey>,
        node: NodeIx,
        work: &mut BuildWork,
    ) {
        if self
            .rows
            .insert(FlatTermEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node_key,
                node,
            })
            .is_some()
        {
            return;
        }
        let next = self
            .visibilities
            .get(&visibility)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.visibilities.insert(visibility.clone(), next);
        if let Some(counts) = self.promoted_counts.get_mut(&key) {
            if let Some((_, count)) = counts.iter_mut().find(|(held, _)| held == &visibility) {
                *count = count.saturating_add(1);
            } else {
                counts.push((visibility, 1));
                counts.sort_by(|left, right| left.0.cmp(&right.0));
            }
        } else {
            let counts = self.raw_counts(&key, FLAT_COUNT_PROMOTION);
            if counts
                .iter()
                .map(|(_, count)| usize::try_from(*count).unwrap_or(usize::MAX))
                .sum::<usize>()
                > FLAT_COUNT_PROMOTION
            {
                self.promoted_counts.insert(key, counts);
            }
        }
        work.postings_inserted = work.postings_inserted.saturating_add(1);
    }

    fn remove(
        &mut self,
        key: &TermKey,
        visibility: &VisibilityIx,
        node_key: &Arc<NodeKey>,
        node: NodeIx,
        work: &mut BuildWork,
    ) {
        if self
            .rows
            .remove(&FlatTermEntry {
                key: key.clone(),
                visibility: visibility.clone(),
                node_key: node_key.clone(),
                node,
            })
            .is_none()
        {
            return;
        }
        if let Some(counts) = self.promoted_counts.get_mut(key) {
            if let Some(position) = counts.iter().position(|(held, _)| held == visibility) {
                counts[position].1 = counts[position].1.saturating_sub(1);
                if counts[position].1 == 0 {
                    counts.remove(position);
                }
            }
            if counts.is_empty() {
                self.promoted_counts.remove(key);
            }
        }
        if let Some(count) = self.visibilities.get(visibility).copied() {
            if count <= 1 {
                self.visibilities.remove(visibility);
            } else {
                self.visibilities.insert(visibility.clone(), count - 1);
            }
        }
        work.postings_removed = work.postings_removed.saturating_add(1);
    }

    fn count(&self, key: &TermKey, admitted: &impl Fn(VisibilityIx) -> bool) -> usize {
        if let Some(counts) = self.promoted_counts.get(key) {
            return counts
                .iter()
                .filter(|(visibility, _)| admitted(*visibility))
                .map(|(_, count)| usize::try_from(*count).unwrap_or(usize::MAX))
                .sum();
        }
        self.visibilities
            .iter()
            .filter(|(visibility, _)| admitted(**visibility))
            .map(|(visibility, _)| self.range_for(key, visibility, None).count())
            .sum()
    }

    fn visit(
        &self,
        key: &TermKey,
        resume: Option<&NodeKey>,
        limit: usize,
        admitted: &impl Fn(VisibilityIx) -> bool,
        mut visit: impl FnMut(OrderedNode) -> bool,
    ) -> usize {
        let available = self.count(key, admitted);
        let mut iterators = self
            .visibilities
            .iter()
            .filter(|(visibility, _)| admitted(**visibility))
            .map(|(visibility, _)| self.range_for(key, visibility, resume))
            .collect::<Vec<_>>();
        let mut pending = BinaryHeap::new();
        for (partition, iterator) in iterators.iter_mut().enumerate() {
            if let Some(entry) = iterator.next() {
                pending.push(Reverse((entry.node_key.clone(), entry.node, partition)));
            }
        }
        let mut visited = 0usize;
        while visited < limit {
            let Some(Reverse((node_key, node, partition))) = pending.pop() else {
                break;
            };
            visited = visited.saturating_add(1);
            if !visit(OrderedNode {
                key: node_key,
                node,
            }) {
                break;
            }
            if let Some(entry) = iterators[partition].next() {
                pending.push(Reverse((entry.node_key.clone(), entry.node, partition)));
            }
        }
        available
    }
}

/// One entry in a field-local ordered value posting. Values and Field names
/// are interned elsewhere; the ordered tree repeats only two `Arc` words and a
/// four-byte generation-local identity per entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FieldValue {
    value: Arc<Value>,
    /// Interned canonical tie-break. This is one shared pointer, not a repeated
    /// NodeKey allocation; observable pages therefore do not depend on slot
    /// allocation history.
    key: Arc<NodeKey>,
    node: NodeIx,
}

type PartitionedOrderedField = Partitioned<FieldValue>;

#[derive(Debug, Clone)]
struct Intern<T> {
    values: ChunkedDirectory<Arc<T>, (u32, u32)>,
    slots: PersistentVector<Option<Arc<T>>>,
    free: PersistentSet<u32>,
}

impl<T> Default for Intern<T> {
    fn default() -> Self {
        Self {
            values: ChunkedDirectory::default(),
            slots: PersistentVector::new(),
            free: PersistentSet::new(),
        }
    }
}

impl<T: Clone + Ord> Intern<T> {
    fn get(&self, value: &T) -> Option<Arc<T>> {
        self.values
            .get_key_value(value)
            .map(|(held, _)| held.clone())
    }

    fn index(&self, value: &T) -> Option<u32> {
        self.values.get(value).map(|(_, index)| *index)
    }

    fn by_index(&self, index: u32) -> Option<&Arc<T>> {
        self.slots.get(index as usize)?.as_ref()
    }

    fn intern(&mut self, value: T) -> Arc<T> {
        if let Some((held, count, index)) = self
            .values
            .get_key_value(&value)
            .map(|(held, (count, index))| (held.clone(), *count, *index))
        {
            self.values
                .insert(held.clone(), (count.saturating_add(1), index));
            return held;
        }
        let value = Arc::new(value);
        let index = if let Some(index) = self.free.iter().next().copied() {
            self.free.remove(&index);
            if let Some(slot) = self.slots.get_mut(index as usize) {
                *slot = Some(value.clone());
            }
            index
        } else {
            let index = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
            self.slots.push_back(Some(value.clone()));
            index
        };
        self.values.insert(value.clone(), (1, index));
        value
    }

    fn release(&mut self, value: &T) {
        let Some((held, count, index)) = self
            .values
            .get_key_value(value)
            .map(|(held, (count, index))| (held.clone(), *count, *index))
        else {
            return;
        };
        if count > 1 {
            self.values.insert(held, (count - 1, index));
        } else {
            self.values.remove(value);
            if let Some(slot) = self.slots.get_mut(index as usize) {
                *slot = None;
            }
            self.free.insert(index);
        }
    }
}

#[derive(Debug, Clone)]
struct BytesIntern {
    values: ChunkedDirectory<Arc<[u8]>, (u32, u32)>,
    slots: PersistentVector<Option<Arc<[u8]>>>,
    free: PersistentSet<u32>,
}

impl Default for BytesIntern {
    fn default() -> Self {
        Self {
            values: ChunkedDirectory::default(),
            slots: PersistentVector::new(),
            free: PersistentSet::new(),
        }
    }
}

impl BytesIntern {
    fn get(&self, value: &[u8]) -> Option<Arc<[u8]>> {
        self.values
            .get_key_value(value)
            .map(|(held, _)| held.clone())
    }

    fn index(&self, value: &[u8]) -> Option<u32> {
        self.values.get(value).map(|(_, index)| *index)
    }

    fn intern(&mut self, value: Arc<[u8]>) -> Arc<[u8]> {
        if let Some((held, count, index)) = self
            .values
            .get_key_value(value.as_ref())
            .map(|(held, (count, index))| (held.clone(), *count, *index))
        {
            self.values
                .insert(held.clone(), (count.saturating_add(1), index));
            return held;
        }
        let index = if let Some(index) = self.free.iter().next().copied() {
            self.free.remove(&index);
            if let Some(slot) = self.slots.get_mut(index as usize) {
                *slot = Some(value.clone());
            }
            index
        } else {
            let index = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
            self.slots.push_back(Some(value.clone()));
            index
        };
        self.values.insert(value.clone(), (1, index));
        value
    }

    fn release(&mut self, value: &[u8]) {
        let Some((held, count, index)) = self
            .values
            .get_key_value(value)
            .map(|(held, (count, index))| (held.clone(), *count, *index))
        else {
            return;
        };
        if count > 1 {
            self.values.insert(held, (count - 1, index));
        } else {
            self.values.remove(value);
            if let Some(slot) = self.slots.get_mut(index as usize) {
                *slot = None;
            }
            self.free.insert(index);
        }
    }
}

/// One ready immutable corpus.
#[derive(Debug, Clone)]
pub(crate) struct Corpus {
    coordinate: WorldPublicationId,
    limits: Limits,
    /// The only Body dictionary for this corpus. BodyIx values are never
    /// meaningful without this exact immutable publication.
    snapshot: Arc<replica::ReadSnapshot>,
    packed: PackedIndex,
    body_rows: PersistentVector<Option<BodyRows>>,
    body_count: usize,
    nodes: ChunkedDirectory<Arc<NodeKey>, NodeIx>,
    node_rows: PersistentVector<Option<NodeColumn>>,
    free_nodes: PersistentSet<NodeIx>,
    schemas: PersistentMap<Arc<SchemaRef>, PartitionedOrderedNodes>,
    ordered_fields: PersistentMap<Arc<FieldRef>, PartitionedOrderedField>,
    exact: FlatExactIndex,
    /// Exact-token and bounded-prefix postings, each canonically ordered by
    /// NodeKey. Prefix postings are materialized once per publication so a
    /// page never unions or rescans a term dictionary at read time.
    terms: FlatTermIndex,
    features: PersistentMap<Arc<FeatureRef>, PartitionedPosting>,
    /// One global ordered adjacency column, partitioned by node+edge gates.
    /// A map from every target to a singleton posting costs an allocation and
    /// HAMT leaf per link; this B+tree keeps millions of sparse targets dense
    /// while retaining O(log links + returned) reverse traversal.
    incoming: Partitioned<IncomingEntry>,
    schema_names: Intern<SchemaRef>,
    field_names: Intern<FieldRef>,
    edge_names: Intern<EdgeRef>,
    gate_names: Intern<crate::find::GateRef>,
    visibility_names: Intern<Visibility>,
    feature_names: Intern<FeatureRef>,
    node_names: Intern<NodeKey>,
    values: Intern<Value>,
    term_bytes: BytesIntern,
    retained_bytes: u64,
    posting_count: u64,
}

/// Streaming full-publication builder.
///
/// The builder owns the only mutable candidate Corpus and accepts one complete
/// source Body at a time. A caller therefore retains at most one extractor
/// output page instead of an all-Body `BTreeMap<BodyKey, BodyExtraction>` plus
/// the finished Corpus. Errors consume no published state: the builder is
/// simply dropped before [`Self::finish`].
pub(crate) struct CorpusBuilder {
    corpus: Corpus,
    work: BuildWork,
    pending: Vec<BodyExtraction>,
}

impl CorpusBuilder {
    pub fn new(
        coordinate: WorldPublicationId,
        limits: Limits,
        snapshot: Arc<replica::ReadSnapshot>,
    ) -> Self {
        Self {
            corpus: Corpus::empty(coordinate, limits, snapshot),
            work: BuildWork::default(),
            pending: Vec::with_capacity(PACKED_SEGMENT_BODIES),
        }
    }

    pub fn push(&mut self, body: BodyExtraction) -> Result<(), Failure> {
        validate_extraction(&body, self.corpus.limits)?;
        if self
            .corpus
            .snapshot
            .body_ix(&body.body)
            .and_then(|source| self.corpus.packed.bodies.get(source.as_u32() as usize))
            .is_some_and(Option::is_some)
        {
            return Err(Failure::DuplicateBody(body.body));
        }
        self.pending.push(body);
        if self.pending.len() == PACKED_SEGMENT_BODIES {
            self.flush()?;
        }
        if self.corpus.retained_bytes > self.corpus.limits.retained_bytes {
            return Err(Failure::Limit("corpus retained bytes"));
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Failure> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let segment =
            PackedSegment::build(&self.corpus.snapshot, std::mem::take(&mut self.pending))?;
        self.pending = Vec::with_capacity(PACKED_SEGMENT_BODIES);
        let work = self.corpus.packed.insert_segment(segment)?;
        self.work.bodies_replaced = self
            .work
            .bodies_replaced
            .saturating_add(work.bodies_replaced);
        self.work.nodes_inserted = self.work.nodes_inserted.saturating_add(work.nodes_inserted);
        self.work.postings_inserted = self
            .work
            .postings_inserted
            .saturating_add(work.postings_inserted);
        self.corpus.retained_bytes = self.corpus.packed.retained_bytes;
        self.corpus.posting_count = self.corpus.packed.posting_count;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(Corpus, BuildWork), Failure> {
        self.flush()?;
        self.work.retained_bytes = self.corpus.retained_bytes;
        Ok((self.corpus, self.work))
    }
}

impl Corpus {
    fn empty(
        coordinate: WorldPublicationId,
        limits: Limits,
        snapshot: Arc<replica::ReadSnapshot>,
    ) -> Self {
        Self {
            coordinate,
            limits,
            snapshot,
            packed: PackedIndex::default(),
            body_rows: PersistentVector::new(),
            body_count: 0,
            nodes: ChunkedDirectory::default(),
            node_rows: PersistentVector::new(),
            free_nodes: PersistentSet::new(),
            schemas: PersistentMap::new(),
            ordered_fields: PersistentMap::new(),
            exact: FlatExactIndex::default(),
            terms: FlatTermIndex::default(),
            features: PersistentMap::new(),
            incoming: Partitioned::default(),
            schema_names: Intern::default(),
            field_names: Intern::default(),
            edge_names: Intern::default(),
            gate_names: Intern::default(),
            visibility_names: Intern::default(),
            feature_names: Intern::default(),
            node_names: Intern::default(),
            values: Intern::default(),
            term_bytes: BytesIntern::default(),
            retained_bytes: 0,
            posting_count: 0,
        }
    }

    /// Build a complete corpus from explicit extractor rows.
    pub fn build(
        coordinate: WorldPublicationId,
        limits: Limits,
        snapshot: Arc<replica::ReadSnapshot>,
        bodies: Vec<BodyExtraction>,
    ) -> Result<(Self, BuildWork), Failure> {
        let mut bodies = bodies;
        bodies.sort_by(|left, right| left.body.cmp(&right.body));
        let mut builder = CorpusBuilder::new(coordinate, limits, snapshot);
        for body in bodies {
            builder.push(body)?;
        }
        builder.finish()
    }

    /// Apply complete replacements for only the Bodies named by `delta`.
    ///
    /// The original corpus is unchanged on every error. Empty replacement
    /// batches are permitted because a Space-wide Manifest root or World
    /// implementation can move while this World's extracted rows remain
    /// byte-equivalent and therefore fully shared.
    pub fn apply(&self, delta: CorpusDelta) -> Result<(Self, BuildWork), Failure> {
        if !self.packed.segments.is_empty() || self.body_count() == 0 {
            self.apply_packed(delta)
        } else {
            self.apply_inner(delta)
        }
    }

    fn apply_packed(&self, delta: CorpusDelta) -> Result<(Self, BuildWork), Failure> {
        if delta.base != self.coordinate {
            return Err(Failure::CoordinateMismatch {
                expected: self.coordinate,
                actual: delta.base,
            });
        }
        let mut bodies = delta.bodies;
        bodies.sort_by(|left, right| left.body.cmp(&right.body));
        let mut changed = BTreeSet::new();
        for body in &bodies {
            if !changed.insert(body.body.clone()) {
                return Err(Failure::DuplicateBody(body.body.clone()));
            }
            validate_extraction(body, self.limits)?;
        }
        let mut next = self.clone();
        next.coordinate = delta.next;
        let mut work = BuildWork {
            bodies_replaced: usize_u64(bodies.len()),
            ..BuildWork::default()
        };
        for body in &bodies {
            let Some(source) = self.snapshot.body_ix(&body.body) else {
                continue;
            };
            if let Some((nodes, postings, retained)) = next.packed.body_work(source) {
                work.nodes_removed = work.nodes_removed.saturating_add(nodes);
                work.postings_removed = work.postings_removed.saturating_add(postings);
                next.packed.remove_body(source);
                next.retained_bytes = next.retained_bytes.saturating_sub(retained);
                next.posting_count = next.posting_count.saturating_sub(postings);
            }
        }
        next.snapshot = delta.snapshot;
        for chunk in bodies.chunks(PACKED_SEGMENT_BODIES) {
            let live = chunk
                .iter()
                .filter(|body| !body.nodes.is_empty())
                .cloned()
                .collect::<Vec<_>>();
            if live.is_empty() {
                continue;
            }
            let segment = PackedSegment::build(&next.snapshot, live)?;
            let inserted = next.packed.insert_segment(segment)?;
            work.nodes_inserted = work.nodes_inserted.saturating_add(inserted.nodes_inserted);
            work.postings_inserted = work
                .postings_inserted
                .saturating_add(inserted.postings_inserted);
            next.retained_bytes = next.retained_bytes.saturating_add(inserted.retained_bytes);
            next.posting_count = next
                .posting_count
                .saturating_add(inserted.postings_inserted);
        }
        if next.retained_bytes > next.limits.retained_bytes {
            return Err(Failure::Limit("corpus retained bytes"));
        }
        work.retained_bytes = next.retained_bytes;
        Ok((next, work))
    }

    fn apply_inner(&self, delta: CorpusDelta) -> Result<(Self, BuildWork), Failure> {
        if delta.base != self.coordinate {
            return Err(Failure::CoordinateMismatch {
                expected: self.coordinate,
                actual: delta.base,
            });
        }

        let mut bodies = delta.bodies;
        bodies.sort_by(|left, right| left.body.cmp(&right.body));
        let mut changed = BTreeSet::new();
        for body in &bodies {
            if !changed.insert(body.body.clone()) {
                return Err(Failure::DuplicateBody(body.body.clone()));
            }
            validate_extraction(body, self.limits)?;
        }

        let mut next = self.clone();
        next.coordinate = delta.next;
        let mut work = BuildWork::default();
        work.bodies_replaced = usize_u64(bodies.len());

        // Remove all changed sources first. A node may move between changed
        // Bodies in one delta without being mistaken for a collision with its
        // prior source.
        for body in &bodies {
            if let Some(old) = next.body_rows_for(&body.body) {
                next.remove_body(&body.body, &old, &mut work);
            }
        }
        // BodyIx is scoped to its exact ReadSnapshot. Retraction above must
        // resolve through the base directory; only insertion may use the next
        // directory (which can legitimately reuse a deleted slot for a new
        // Body).
        next.snapshot = delta.snapshot;
        for body in bodies {
            next.insert_body(body, &mut work)?;
        }
        if next.retained_bytes > next.limits.retained_bytes {
            return Err(Failure::Limit("corpus retained bytes"));
        }
        work.retained_bytes = next.retained_bytes;
        Ok((next, work))
    }

    fn remove_body(&mut self, body: &BodyKey, rows: &BodyRows, work: &mut BuildWork) {
        let Some(body_ix) = self.snapshot.body_ix(body) else {
            return;
        };
        self.retained_bytes = self.retained_bytes.saturating_sub(rows.retained_bytes);
        let postings_before = work.postings_removed;
        for node_ix in rows.nodes.iter().copied() {
            let Some((_, column)) = self.row(node_ix) else {
                continue;
            };
            let column = column.clone();
            let key = &column.key;
            let node_visibility = Visibility::node(column.gate.clone());
            self.nodes.remove(key.as_ref());
            work.nodes_removed = work.nodes_removed.saturating_add(1);
            if let Some(schema) = self.schema_names.get(&key.schema) {
                partitioned_remove(
                    &mut self.schemas,
                    &schema,
                    &node_visibility,
                    &OrderedNode {
                        key: key.clone(),
                        node: node_ix,
                    },
                    work,
                );
            }
            for field in column.fields.iter() {
                let visibility = Visibility::member(column.gate.clone(), field.gate.clone());
                partitioned_remove(
                    &mut self.ordered_fields,
                    &field.reference,
                    &visibility,
                    &FieldValue {
                        value: field.value.clone(),
                        key: key.clone(),
                        node: node_ix,
                    },
                    work,
                );
                let field_ix = self.field_names.index(field.reference.as_ref());
                let value_ix = self.values.index(field.value.as_ref());
                let visibility_ix = self.visibility_names.index(&visibility);
                if let (Some(field_ix), Some(value_ix), Some(visibility_ix)) =
                    (field_ix, value_ix, visibility_ix)
                {
                    self.exact.remove(
                        &ExactKey {
                            field: field_ix,
                            value: value_ix,
                        },
                        &VisibilityIx(visibility_ix),
                        node_ix,
                        work,
                    );
                    self.visibility_names.release(&visibility);
                }
                for term in field.terms.iter() {
                    if let (Some(term_ix), Some(visibility_ix)) = (
                        self.term_bytes.index(term.as_ref()),
                        self.visibility_names.index(&visibility),
                    ) {
                        self.terms.remove(
                            &TermKey {
                                field: field.reference.clone(),
                                term: term_ix,
                            },
                            &VisibilityIx(visibility_ix),
                            key,
                            node_ix,
                            work,
                        );
                        self.visibility_names.release(&visibility);
                    }
                    self.term_bytes.release(term.as_ref());
                }
                self.field_names.release(field.reference.as_ref());
                self.values.release(field.value.as_ref());
                if let Some(gate) = &field.gate {
                    self.gate_names.release(gate.as_ref());
                }
            }
            for feature in column.features.iter() {
                partitioned_remove(
                    &mut self.features,
                    &feature.reference,
                    &Visibility::member(column.gate.clone(), feature.gate.clone()),
                    &node_ix,
                    work,
                );
                self.feature_names.release(feature.reference.as_ref());
                if let Some(gate) = &feature.gate {
                    self.gate_names.release(gate.as_ref());
                }
            }
            for edge in column.edges.iter() {
                for target in edge.targets.iter() {
                    partition_remove(
                        &mut self.incoming,
                        &Visibility::member(column.gate.clone(), Some(edge.gate.clone())),
                        &IncomingEntry {
                            edge: edge.reference.clone(),
                            target: target.clone(),
                            source: node_ix,
                        },
                        work,
                    );
                    self.node_names.release(target.as_ref());
                }
                self.edge_names.release(edge.reference.as_ref());
                self.gate_names.release(edge.gate.as_ref());
            }
            if let Some(gate) = &column.gate {
                self.gate_names.release(gate.as_ref());
            }
            self.schema_names.release(&key.schema);
            self.node_names.release(key.as_ref());
            self.clear_node_slot(node_ix);
        }
        self.clear_body_slot(body_ix);
        self.posting_count = self
            .posting_count
            .saturating_sub(work.postings_removed.saturating_sub(postings_before));
    }

    fn insert_body(
        &mut self,
        mut body: BodyExtraction,
        work: &mut BuildWork,
    ) -> Result<(), Failure> {
        canonicalize_extraction(&mut body);
        if self.body_rows_for(&body.body).is_some() {
            return Err(Failure::DuplicateBody(body.body));
        }
        let Some(body_ix) = self.snapshot.body_ix(&body.body) else {
            // A changed Body absent from the next exact snapshot is a
            // tombstone. Extractors represent it as an empty replacement.
            return if body.nodes.is_empty() {
                Ok(())
            } else {
                Err(Failure::Invalid("extracted Body absent from snapshot"))
            };
        };
        let mut node_ixs = Vec::with_capacity(body.nodes.len());
        let mut body_bytes = usize_u64(body.stamp.len());
        let postings_before = work.postings_inserted;

        for row in body.nodes {
            if self.nodes.get(&row.key).is_some() {
                return Err(Failure::DuplicateNode(row.key));
            }
            body_bytes = body_bytes.saturating_add(retained_node_bytes(&row));
            let node_ix = self.allocate_node_slot()?;
            let key = self.node_names.intern(row.key);
            let schema = self.schema_names.intern(key.schema.clone());
            let node_gate = row.gate.map(|gate| self.gate_names.intern(gate));
            let node_visibility = Visibility::node(node_gate.clone());
            let mut fields = Vec::with_capacity(row.fields.len());
            let mut edges = Vec::with_capacity(row.edges.len());
            let mut features = Vec::with_capacity(row.features.len());

            partitioned_insert(
                &mut self.schemas,
                schema.clone(),
                node_visibility.clone(),
                OrderedNode {
                    key: key.clone(),
                    node: node_ix,
                },
                work,
            );
            for field in row.fields {
                let reference = self.field_names.intern(field.reference);
                let value = self.values.intern(field.value);
                let field_gate = field.gate.map(|gate| self.gate_names.intern(gate));
                let visibility = Visibility::member(node_gate.clone(), field_gate.clone());
                let terms: Vec<Arc<[u8]>> = field
                    .terms
                    .into_iter()
                    .map(|term| self.term_bytes.intern(term))
                    .collect();
                partitioned_insert(
                    &mut self.ordered_fields,
                    reference.clone(),
                    visibility.clone(),
                    FieldValue {
                        value: value.clone(),
                        key: key.clone(),
                        node: node_ix,
                    },
                    work,
                );
                self.exact.insert(
                    ExactKey {
                        field: self
                            .field_names
                            .index(reference.as_ref())
                            .ok_or(Failure::Invalid("missing interned Field"))?,
                        value: self
                            .values
                            .index(value.as_ref())
                            .ok_or(Failure::Invalid("missing interned Value"))?,
                    },
                    VisibilityIx({
                        self.visibility_names.intern(visibility.clone());
                        self.visibility_names
                            .index(&visibility)
                            .ok_or(Failure::Invalid("missing interned visibility"))?
                    }),
                    node_ix,
                    work,
                );
                for term in &terms {
                    self.terms.insert(
                        TermKey {
                            field: reference.clone(),
                            term: self
                                .term_bytes
                                .index(term.as_ref())
                                .ok_or(Failure::Invalid("missing interned Term"))?,
                        },
                        VisibilityIx({
                            self.visibility_names.intern(visibility.clone());
                            self.visibility_names
                                .index(&visibility)
                                .ok_or(Failure::Invalid("missing interned visibility"))?
                        }),
                        key.clone(),
                        node_ix,
                        work,
                    );
                }
                fields.push(StoredField {
                    reference,
                    value,
                    gate: field_gate,
                    terms: InlineRows::from_vec(terms),
                });
            }
            for feature in row.features {
                let reference = self.feature_names.intern(feature.reference);
                let feature_gate = feature.gate.map(|gate| self.gate_names.intern(gate));
                partitioned_insert(
                    &mut self.features,
                    reference.clone(),
                    Visibility::member(node_gate.clone(), feature_gate.clone()),
                    node_ix,
                    work,
                );
                features.push(StoredFeature {
                    reference,
                    gate: feature_gate,
                    value: feature.value,
                });
            }
            for edge in row.edges {
                let reference = self.edge_names.intern(edge.reference);
                let gate = self.gate_names.intern(edge.gate);
                let targets: Vec<Arc<NodeKey>> = edge
                    .targets
                    .into_iter()
                    .map(|target| self.node_names.intern(target))
                    .collect();
                for target in &targets {
                    partition_insert(
                        &mut self.incoming,
                        Visibility::member(node_gate.clone(), Some(gate.clone())),
                        IncomingEntry {
                            edge: reference.clone(),
                            target: target.clone(),
                            source: node_ix,
                        },
                        work,
                    );
                }
                edges.push(StoredEdge {
                    reference,
                    gate,
                    targets: InlineRows::from_vec(targets),
                });
            }
            self.set_node_slot(
                node_ix,
                NodeColumn {
                    body: body_ix,
                    key: key.clone(),
                    gate: node_gate,
                    fields: InlineRows::from_vec(fields),
                    edges: InlineRows::from_vec(edges),
                    features: InlineRows::from_vec(features),
                },
            )?;
            if self.nodes.insert(key, node_ix).is_some() {
                return Err(Failure::Invalid("node index replacement"));
            }
            node_ixs.push(node_ix);
            work.nodes_inserted = work.nodes_inserted.saturating_add(1);
        }

        let rows = BodyRows {
            nodes: BodyNodes::from_vec(node_ixs),
            retained_bytes: body_bytes,
        };
        self.set_body_slot(body_ix, rows)?;
        self.body_count = self.body_count.saturating_add(1);
        self.retained_bytes = self.retained_bytes.saturating_add(body_bytes);
        self.posting_count = self
            .posting_count
            .saturating_add(work.postings_inserted.saturating_sub(postings_before));
        Ok(())
    }

    fn body_rows_for(&self, body: &BodyKey) -> Option<BodyRows> {
        let body_ix = self.snapshot.body_ix(body)?;
        self.body_rows
            .get(body_ix.as_u32() as usize)?
            .as_ref()
            .cloned()
    }

    fn set_body_slot(&mut self, index: BodyIx, rows: BodyRows) -> Result<(), Failure> {
        let needed = index.as_u32() as usize;
        while self.body_rows.len() <= needed {
            self.body_rows.push_back(None);
        }
        let slot = self
            .body_rows
            .get_mut(needed)
            .ok_or(Failure::Invalid("Body slot"))?;
        if slot.replace(rows).is_some() {
            return Err(Failure::Invalid("occupied Body slot"));
        }
        Ok(())
    }

    fn clear_body_slot(&mut self, index: BodyIx) {
        if let Some(slot) = self.body_rows.get_mut(index.as_u32() as usize) {
            if slot.take().is_some() {
                self.body_count = self.body_count.saturating_sub(1);
            }
        }
    }

    fn allocate_node_slot(&mut self) -> Result<NodeIx, Failure> {
        if let Some(index) = self.free_nodes.remove_min() {
            let Some(slot) = self.node_rows.get_mut(index.0 as usize) else {
                return Err(Failure::Invalid("Node slot"));
            };
            if slot.is_some() {
                return Err(Failure::Invalid("occupied Node slot"));
            }
            return Ok(index);
        }
        let raw = u32::try_from(self.node_rows.len())
            .map_err(|_| Failure::Limit("corpus Node identities"))?;
        self.node_rows.push_back(None);
        Ok(NodeIx(raw))
    }

    fn set_node_slot(&mut self, index: NodeIx, row: NodeColumn) -> Result<(), Failure> {
        let slot = self
            .node_rows
            .get_mut(index.0 as usize)
            .ok_or(Failure::Invalid("Node slot"))?;
        if slot.replace(row).is_some() {
            return Err(Failure::Invalid("occupied Node slot"));
        }
        Ok(())
    }

    fn clear_node_slot(&mut self, index: NodeIx) {
        if let Some(slot) = self.node_rows.get_mut(index.0 as usize) {
            *slot = None;
            self.free_nodes.insert(index);
        }
    }

    pub const fn coordinate(&self) -> WorldPublicationId {
        self.coordinate
    }

    pub(crate) fn snapshot(&self) -> Arc<replica::ReadSnapshot> {
        self.snapshot.clone()
    }

    pub fn body_count(&self) -> usize {
        if self.packed.segments.is_empty() {
            self.body_count
        } else {
            self.packed.body_count
        }
    }

    pub fn node_count(&self) -> usize {
        if self.packed.segments.is_empty() {
            self.nodes.len()
        } else {
            self.packed.node_count
        }
    }

    pub const fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }

    /// O(1) conservative physical-retention price for admission and cache
    /// policy. The fixed terms are calibrated above the release-scale dense
    /// and one-record-per-Body fixtures after the compact directory cutover;
    /// logical extractor bytes remain exact.
    pub fn retained_bytes_estimate(&self) -> u64 {
        if !self.packed.segments.is_empty() {
            return self
                .packed
                .physical_bytes
                .saturating_add(usize_u64(self.packed.nodes.len()).saturating_mul(112))
                .saturating_add(usize_u64(self.packed.bodies.len()).saturating_mul(16))
                .saturating_add(usize_u64(self.packed.stale.len()).saturating_mul(24))
                .saturating_add(2 * 1024 * 1024);
        }
        self.retained_bytes
            .saturating_add(
                u64::try_from(self.node_count())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(192),
            )
            .saturating_add(
                u64::try_from(self.body_count())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(40),
            )
            .saturating_add(self.posting_count.saturating_mul(112))
    }

    /// Conservative pre-build price derived from identity-bound extractor
    /// declarations and exact source-schema counts in `snapshot`.
    ///
    /// Full publication construction streams one source Body at a time. The
    /// retained component therefore prices every admitted row and posting,
    /// while the transient component prices only the largest combined source
    /// extraction plus fixed builder scratch. This is independent of actual
    /// extractor output and can be reserved before any World code runs.
    pub(crate) fn estimate_build_bytes(
        snapshot: &replica::ReadSnapshot,
        world: &replica::body::WorldId,
        extractors: &[crate::find::Extractor],
    ) -> BuildMemory {
        let mut by_source =
            BTreeMap::<crate::find::SourceRef, Vec<crate::find::ExtractionShape>>::new();
        for extractor in extractors {
            by_source
                .entry(extractor.source.clone())
                .or_default()
                .push(extractor.shape);
        }

        let mut retained = 0u64;
        let mut max_body_transient = 0u64;
        for (source, shapes) in by_source {
            let bodies =
                snapshot.body_count_with_schema_version(world, &source.name, source.version);
            let source_bytes = snapshot.body_payload_bytes_with_schema_version(
                world,
                &source.name,
                source.version,
            );
            let (source_retained, body_transient, _) = growth_price(&shapes, bodies, source_bytes);
            retained = retained.saturating_add(source_retained);
            max_body_transient = max_body_transient.max(body_transient);
        }
        BuildMemory {
            retained_bytes: retained,
            transient_bytes: max_body_transient.saturating_add(8 * 1024 * 1024),
        }
    }

    /// Conservative pre-reconstruction price for an exact durable generation.
    ///
    /// `footprint` is authenticated beside the requested generation root and
    /// contains the source counts/bytes needed by the same extractor-growth
    /// calculation as [`Self::estimate_build_bytes`]. No ambient/current
    /// snapshot participates, so a larger historical generation cannot be
    /// admitted using a smaller current publication's cost.
    pub(crate) fn estimate_build_bytes_from_footprint(
        footprint: &replica::GenerationFootprint,
        world: &replica::body::WorldId,
        extractors: &[crate::find::Extractor],
    ) -> BuildMemory {
        let mut by_source =
            BTreeMap::<crate::find::SourceRef, Vec<crate::find::ExtractionShape>>::new();
        for extractor in extractors {
            by_source
                .entry(extractor.source.clone())
                .or_default()
                .push(extractor.shape);
        }

        let mut retained = 0u64;
        let mut max_body_transient = 0u64;
        for (source, shapes) in by_source {
            let aggregate = footprint.sources.iter().find(|aggregate| {
                &aggregate.world == world
                    && aggregate.schema == source.name
                    && aggregate.version == source.version
            });
            let bodies = aggregate.map_or(0, |aggregate| aggregate.body_count);
            let source_bytes = aggregate.map_or(0, |aggregate| aggregate.payload_bytes);
            let (source_retained, body_transient, _) = growth_price(&shapes, bodies, source_bytes);
            retained = retained.saturating_add(source_retained);
            max_body_transient = max_body_transient.max(body_transient);
        }
        BuildMemory {
            retained_bytes: retained,
            transient_bytes: max_body_transient.saturating_add(8 * 1024 * 1024),
        }
    }

    /// Additional physical headroom for one structurally-shared delta build.
    ///
    /// The base Corpus remains resident while the candidate is assembled. Only
    /// changed Body rows and posting leaves are newly retained; unchanged
    /// directories and columns remain Arc-identical. Deleted Bodies are priced
    /// from the base snapshot, inserted/replaced Bodies from `next_snapshot`.
    pub(crate) fn estimate_delta_build_bytes(
        &self,
        next_snapshot: &replica::ReadSnapshot,
        world: &replica::body::WorldId,
        extractors: &[crate::find::Extractor],
        changed: &[BodyKey],
    ) -> BuildMemory {
        let mut by_source =
            BTreeMap::<(replica::body::SchemaId, u32), Vec<crate::find::ExtractionShape>>::new();
        for extractor in extractors {
            by_source
                .entry((extractor.source.name.clone(), extractor.source.version))
                .or_default()
                .push(extractor.shape);
        }

        let mut retained = 0u64;
        let mut max_body_transient = 0u64;
        let mut seen = BTreeSet::new();
        for key in changed {
            if &key.world != world || !seen.insert(key) {
                continue;
            }
            let binding = next_snapshot
                .binding(key)
                .or_else(|| self.snapshot.binding(key));
            let Some(binding) = binding else {
                continue;
            };
            let Some(shapes) = by_source.get(&(binding.schema.clone(), binding.schema_version))
            else {
                continue;
            };
            let source_bytes = next_snapshot
                .body_payload_bytes(key)
                .or_else(|| self.snapshot.body_payload_bytes(key))
                .unwrap_or(0);
            let (body_retained, body_transient, postings) = growth_price(shapes, 1, source_bytes);
            // A changed posting rewrites at most one 256-entry immutable leaf
            // plus its shallow vector spine. This deliberately overprices all
            // posting kinds at the widest observed flat-entry layout.
            let path_copy = 32u64
                .saturating_mul(1024)
                .saturating_add(postings.saturating_mul(32 * 1024));
            retained = retained
                .saturating_add(body_retained)
                .saturating_add(path_copy);
            max_body_transient = max_body_transient.max(body_transient);
        }
        BuildMemory {
            retained_bytes: retained,
            transient_bytes: max_body_transient.saturating_add(2 * 1024 * 1024),
        }
    }

    pub fn body_stamp(&self, body: &BodyKey) -> Option<Arc<[u8]>> {
        if self.packed.segments.is_empty() {
            self.body_rows_for(body)?;
        } else {
            let body_ix = self.snapshot.body_ix(body)?;
            self.packed
                .bodies
                .get(body_ix.as_u32() as usize)?
                .as_ref()?;
        }
        self.snapshot.body_stamp(body).map(Arc::from)
    }

    fn visibility_admitted(
        &self,
        visibility: VisibilityIx,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> bool {
        self.visibility_names
            .by_index(visibility.0)
            .is_some_and(|visibility| visibility.admitted(allows))
    }

    pub fn node(&self, key: &NodeKey) -> Option<ExtractedNode> {
        if !self.packed.segments.is_empty() {
            return self
                .packed
                .node_ref(key)
                .and_then(|reference| self.packed.materialize(reference, &|_| true));
        }
        let index = *self.nodes.get(key)?;
        self.materialize(index)
    }

    /// Materialize only an admitted node and admitted members. Node gating is
    /// checked before any field value, feature payload, or edge target is
    /// cloned; member gates are checked before their payloads are projected.
    pub fn node_admitted(
        &self,
        key: &NodeKey,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> Option<ExtractedNode> {
        if !self.packed.segments.is_empty() {
            return self
                .packed
                .node_ref(key)
                .and_then(|reference| self.packed.materialize(reference, &allows));
        }
        let index = *self.nodes.get(key)?;
        self.materialize_admitted(index, &allows)
    }

    /// Exact admitted cardinality from posting metadata. No row is decoded and
    /// denied partitions are not visited.
    pub fn count_schema(
        &self,
        schema: &SchemaRef,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> usize {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.grouped_ranges(
                |segment| &segment.schema_groups,
                |segment| &segment.schema_nodes,
                None,
                &allows,
                |segment| u16::try_from(segment.schemas.binary_search(schema).ok()?).ok(),
            );
            return self
                .packed
                .count_ranges(&ranges, |segment| &segment.schema_nodes);
        }
        let posting = self
            .schema_names
            .get(schema)
            .and_then(|key| self.schemas.get(&key));
        count_partitioned(posting, &allows)
    }

    pub fn count_exact(
        &self,
        field: &FieldRef,
        value: &Value,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> usize {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.field_ranges(
                field,
                FieldScanBounds::Predicate {
                    test: Test::Equal,
                    value,
                },
                None,
                &allows,
            );
            return self.packed.count_exact_field_ranges(&ranges);
        }
        self.field_names
            .index(field)
            .zip(self.values.index(value))
            .map(|(field, value)| {
                self.exact.count(&ExactKey { field, value }, &|visibility| {
                    self.visibility_admitted(visibility, &allows)
                })
            })
            .unwrap_or(0)
    }

    pub fn count_term(
        &self,
        field: &FieldRef,
        term: &[u8],
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> usize {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.term_ranges(field, term, None, &allows);
            return self
                .packed
                .count_ranges(&ranges, |segment| &segment.term_nodes);
        }
        self.field_names
            .get(field)
            .zip(self.term_bytes.index(term))
            .map(|(field, term)| {
                self.terms.count(&TermKey { field, term }, &|visibility| {
                    self.visibility_admitted(visibility, &allows)
                })
            })
            .unwrap_or(0)
    }

    /// Durable source of one extracted node.
    pub fn source(&self, key: &NodeKey) -> Option<BodyKey> {
        if !self.packed.segments.is_empty() {
            let reference = self.packed.node_ref(key)?;
            let segment = self.packed.segment(reference.segment)?;
            let row = segment.nodes.get(reference.node as usize)?;
            return self.snapshot.body_key(row.source).cloned();
        }
        let index = *self.nodes.get(key)?;
        self.source_for_node(index).cloned()
    }

    /// Visit at most `limit` nodes extracted from one durable Body, narrowed
    /// to the requested Find Schema. Body seeks are the bounded bridge from a
    /// committed change to ordinary query evaluation.
    pub fn visit_body(
        &self,
        body: &BodyKey,
        schema: &SchemaRef,
        limit: usize,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        if !self.packed.segments.is_empty() {
            let Some(source) = self.snapshot.body_ix(body) else {
                return Visit {
                    available: 0,
                    visited: 0,
                };
            };
            let Some(owner) = self
                .packed
                .bodies
                .get(source.as_u32() as usize)
                .and_then(|owner| *owner)
            else {
                return Visit {
                    available: 0,
                    visited: 0,
                };
            };
            let Some(segment) = self.packed.segment(owner.segment) else {
                return Visit {
                    available: 0,
                    visited: 0,
                };
            };
            let Some(row) = segment.body_rows.get(owner.body as usize) else {
                return Visit {
                    available: 0,
                    visited: 0,
                };
            };
            let mut available = 0usize;
            let mut visited = 0usize;
            for node in packed_slice(&segment.body_nodes, row.node_start, row.node_len) {
                let reference = PackedNodeRef {
                    segment: owner.segment,
                    node: *node,
                };
                let Some(key) = segment.node_key(*node) else {
                    continue;
                };
                if key.schema != *schema || !self.packed.is_live(reference) {
                    continue;
                }
                let Some(materialized) = self.packed.materialize(reference, &allows) else {
                    continue;
                };
                available = available.saturating_add(1);
                if visited < limit {
                    visited = visited.saturating_add(1);
                    if !visit(body, &materialized) {
                        break;
                    }
                }
            }
            return Visit { available, visited };
        }
        let Some(rows) = self.body_rows_for(body) else {
            return Visit {
                available: 0,
                visited: 0,
            };
        };
        let available = rows
            .nodes
            .iter()
            .filter(|index| {
                self.row(**index).is_some_and(|(_, column)| {
                    column.key.schema == *schema && allows(column.gate.as_deref())
                })
            })
            .count();
        let mut visited = 0usize;
        for index in rows.nodes.iter().copied().filter(|index| {
            self.row(*index).is_some_and(|(_, column)| {
                column.key.schema == *schema && allows(column.gate.as_deref())
            })
        }) {
            if visited == limit {
                break;
            }
            let Some(node) = self.materialize_admitted(index, &allows) else {
                continue;
            };
            visited = visited.saturating_add(1);
            if !visit(body, &node) {
                break;
            }
        }
        Visit { available, visited }
    }

    /// Visit at most `limit` nodes in one Schema posting.
    pub fn visit_schema(
        &self,
        schema: &SchemaRef,
        limit: usize,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.grouped_ranges(
                |segment| &segment.schema_groups,
                |segment| &segment.schema_nodes,
                None,
                &allows,
                |segment| u16::try_from(segment.schemas.binary_search(schema).ok()?).ok(),
            );
            let available = self
                .packed
                .count_ranges(&ranges, |segment| &segment.schema_nodes);
            let visited = self.packed.scan_ranges(
                ranges,
                |segment| &segment.schema_nodes,
                &allows,
                limit,
                |_, source, node| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(body, node))
                },
            );
            return Visit { available, visited };
        }
        let posting = self
            .schema_names
            .get(schema)
            .and_then(|key| self.schemas.get(&key));
        self.visit_ordered_nodes(posting, limit, &allows, &mut visit)
    }

    /// Visit at most `limit` nodes having one exact Field value.
    pub fn visit_exact(
        &self,
        field: &FieldRef,
        value: &Value,
        limit: usize,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.field_ranges(
                field,
                FieldScanBounds::Predicate {
                    test: Test::Equal,
                    value,
                },
                None,
                &allows,
            );
            let available = self.packed.count_exact_field_ranges(&ranges);
            let visited =
                self.packed
                    .scan_field_ranges(ranges, &allows, limit, |_, _, source, node| {
                        self.snapshot
                            .body_key(source)
                            .is_some_and(|body| visit(body, node))
                    });
            return Visit { available, visited };
        }
        let Some((field, value)) = self.field_names.index(field).zip(self.values.index(value))
        else {
            return Visit {
                available: 0,
                visited: 0,
            };
        };
        let mut visited = 0usize;
        let available = self.exact.visit(
            &ExactKey { field, value },
            limit,
            &|visibility| self.visibility_admitted(visibility, &allows),
            |index| {
                if self.row(index).is_none() {
                    return true;
                }
                let Some(node) = self.materialize_admitted(index, &allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(index) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(source, &node)
            },
        );
        Visit { available, visited }
    }

    /// Visit the narrow ordered interval for a range or prefix predicate.
    /// Equality keeps using the exact hash posting; Contains intentionally
    /// falls back to the same-kind interval because substring indexes are a
    /// separate analyzer concern.
    pub fn visit_field_range(
        &self,
        field: &FieldRef,
        test: Test,
        value: &Value,
        limit: usize,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.field_ranges(
                field,
                FieldScanBounds::Predicate { test, value },
                None,
                &allows,
            );
            let available = self.packed.count_field_ranges(&ranges);
            let visited =
                self.packed
                    .scan_field_ranges(ranges, &allows, limit, |_, _, source, node| {
                        self.snapshot
                            .body_key(source)
                            .is_some_and(|body| visit(body, node))
                    });
            return Visit { available, visited };
        }
        let posting = self
            .field_names
            .get(field)
            .and_then(|key| self.ordered_fields.get(&key));
        let bounds = field_value_bounds(field, test, value);
        self.visit_ordered_field(posting, Some(bounds), limit, &allows, &mut visit)
    }

    /// Visit at most `limit` nodes in one analyzed-term posting.
    pub fn visit_term(
        &self,
        field: &FieldRef,
        term: &[u8],
        prefix: bool,
        limit: usize,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(&BodyKey, &ExtractedNode, u32) -> bool,
    ) -> Visit {
        if prefix {
            return Visit {
                available: 0,
                visited: 0,
            };
        }
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.term_ranges(field, term, None, &allows);
            let available = self
                .packed
                .count_ranges(&ranges, |segment| &segment.term_nodes);
            let visited = self.packed.scan_term_ranges(
                ranges,
                &allows,
                limit,
                |_, source, node, frequency| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(body, node, frequency))
                },
            );
            return Visit { available, visited };
        }
        let Some((field, term)) = self.field_names.get(field).zip(self.term_bytes.index(term))
        else {
            return Visit {
                available: 0,
                visited: 0,
            };
        };
        let mut visited = 0usize;
        let available = self.terms.visit(
            &TermKey { field, term },
            None,
            limit,
            &|visibility| self.visibility_admitted(visibility, &allows),
            |entry| {
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(entry.node) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(source, &node, 1)
            },
        );
        Visit { available, visited }
    }

    /// Visit nodes carrying an augmented feature value. The exact feature
    /// implementation interprets probes and establishes scores; the corpus
    /// remains implementation-neutral derived storage.
    pub fn visit_feature(
        &self,
        feature: &FeatureRef,
        limit: usize,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.grouped_ranges(
                |segment| &segment.feature_groups,
                |segment| &segment.feature_nodes,
                None,
                &allows,
                |segment| u16::try_from(segment.feature_names.binary_search(feature).ok()?).ok(),
            );
            let available = self
                .packed
                .count_ranges(&ranges, |segment| &segment.feature_nodes);
            let visited = self.packed.scan_ranges(
                ranges,
                |segment| &segment.feature_nodes,
                &allows,
                limit,
                |_, source, node| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(body, node))
                },
            );
            return Visit { available, visited };
        }
        let posting = self
            .feature_names
            .get(feature)
            .and_then(|key| self.features.get(&key));
        self.visit_posting(posting, limit, &allows, &mut visit)
    }

    /// Visit sources of one incoming Edge without scanning the Schema.
    pub fn visit_incoming(
        &self,
        edge: &EdgeRef,
        target: &NodeKey,
        limit: usize,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.pair_ranges(
                |segment| &segment.incoming_groups,
                |segment| &segment.incoming_targets,
                |segment| &segment.incoming_nodes,
                None,
                &allows,
                |segment| {
                    let edge = u16::try_from(segment.edge_names.binary_search(edge).ok()?).ok()?;
                    let target = segment.target_key_id(target)?;
                    Some((edge, target))
                },
            );
            let available = self
                .packed
                .count_ranges(&ranges, |segment| &segment.incoming_nodes);
            let visited = self.packed.scan_ranges(
                ranges,
                |segment| &segment.incoming_nodes,
                &allows,
                limit,
                |_, source, node| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(body, node))
                },
            );
            return Visit { available, visited };
        }
        let Some((edge, target)) = self.edge_names.get(edge).zip(self.node_names.get(target))
        else {
            return Visit {
                available: 0,
                visited: 0,
            };
        };
        let mut visited = 0usize;
        visit_partitioned(
            Some(&self.incoming),
            (
                std::ops::Bound::Included(IncomingEntry {
                    edge: edge.clone(),
                    target: target.clone(),
                    source: NodeIx(0),
                }),
                std::ops::Bound::Included(IncomingEntry {
                    edge,
                    target,
                    source: NodeIx(u32::MAX),
                }),
            ),
            &allows,
            limit,
            |entry| {
                if self.row(entry.source).is_none() {
                    return true;
                }
                let Some(node) = self.materialize_admitted(entry.source, &allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(entry.source) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(source, &node)
            },
        );
        Visit {
            available: visited,
            visited,
        }
    }

    /// Scan a Schema posting from an inclusive generation-local identity.
    /// This is the evaluator continuation seam; no earlier posting entry is
    /// materialized or visited.
    pub fn scan_schema(
        &self,
        schema: &SchemaRef,
        resume: Option<NodeKey>,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(NodeKey, &BodyKey, &ExtractedNode) -> bool,
    ) -> usize {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.grouped_ranges(
                |segment| &segment.schema_groups,
                |segment| &segment.schema_nodes,
                resume.as_ref(),
                &allows,
                |segment| u16::try_from(segment.schemas.binary_search(schema).ok()?).ok(),
            );
            return self.packed.scan_ranges(
                ranges,
                |segment| &segment.schema_nodes,
                &allows,
                usize::MAX,
                |key, source, node| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(key, body, node))
                },
            );
        }
        let Some(posting) = self
            .schema_names
            .get(schema)
            .and_then(|key| self.schemas.get(&key))
        else {
            return 0;
        };
        let lower = resume
            .map(|key| {
                let node = self.nodes.get(&key).copied().unwrap_or(NodeIx(0));
                std::ops::Bound::Included(OrderedNode {
                    key: Arc::new(key),
                    node,
                })
            })
            .unwrap_or(std::ops::Bound::Unbounded);
        let mut visited = 0usize;
        visit_partitioned(
            Some(posting),
            (lower, std::ops::Bound::Unbounded),
            &allows,
            usize::MAX,
            |entry| {
                if self.row(entry.node).is_none() {
                    return true;
                }
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(entry.node) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(entry.key.as_ref().clone(), source, &node)
            },
        );
        visited
    }

    /// Scan an exact-token or precomputed prefix posting in canonical NodeKey
    /// order from the first not-yet-returned key.
    pub fn scan_term(
        &self,
        field: &FieldRef,
        term: &[u8],
        prefix: bool,
        resume: Option<NodeKey>,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(NodeKey, &BodyKey, &ExtractedNode, u32) -> bool,
    ) -> usize {
        if prefix {
            return 0;
        }
        if !self.packed.segments.is_empty() {
            let ranges = self
                .packed
                .term_ranges(field, term, resume.as_ref(), &allows);
            return self.packed.scan_term_ranges(
                ranges,
                &allows,
                usize::MAX,
                |key, source, node, frequency| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(key, body, node, frequency))
                },
            );
        }
        let Some((field, term)) = self.field_names.get(field).zip(self.term_bytes.index(term))
        else {
            return 0;
        };
        let mut visited = 0usize;
        self.terms.visit(
            &TermKey { field, term },
            resume.as_ref(),
            usize::MAX,
            &|visibility| self.visibility_admitted(visibility, &allows),
            |entry| {
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(entry.node) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(entry.key.as_ref().clone(), source, &node, 1)
            },
        );
        visited
    }

    /// Scan a field-local B+tree from an inclusive value/identity tuple.
    /// Range and prefix starts seek directly into the relevant leaf.
    pub fn scan_field_range(
        &self,
        field: &FieldRef,
        test: Test,
        value: &Value,
        resume: Option<(Value, NodeKey)>,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(Value, NodeKey, &BodyKey, &ExtractedNode) -> bool,
    ) -> usize {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.field_ranges(
                field,
                FieldScanBounds::Predicate { test, value },
                resume.as_ref(),
                &allows,
            );
            return self.packed.scan_field_ranges(
                ranges,
                &allows,
                usize::MAX,
                |value, key, source, node| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(value, key, body, node))
                },
            );
        }
        let Some(posting) = self
            .field_names
            .get(field)
            .and_then(|key| self.ordered_fields.get(&key))
        else {
            return 0;
        };
        let (mut lower, upper) = field_value_bounds(field, test, value);
        if let Some((value, key)) = resume {
            let node = self.nodes.get(&key).copied().unwrap_or(NodeIx(0));
            lower = later_lower_bound(
                lower,
                std::ops::Bound::Included(FieldValue {
                    value: Arc::new(value),
                    key: Arc::new(key),
                    node,
                }),
            );
        }
        let mut visited = 0usize;
        visit_partitioned(
            Some(posting),
            (lower, upper),
            &allows,
            usize::MAX,
            |entry| {
                if self.row(entry.node).is_none() {
                    return true;
                }
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(entry.node) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(
                    entry.value.as_ref().clone(),
                    entry.key.as_ref().clone(),
                    source,
                    &node,
                )
            },
        );
        visited
    }

    /// Scan one finite field-local interval from its exact lower endpoint and
    /// stop at its exact upper endpoint. Unlike a post-Seek predicate, the
    /// upper bound terminates posting work before unrelated later values are
    /// visited. `resume` is the same inclusive value/identity tuple used by
    /// ordinary ordered Field pagination.
    pub fn scan_field_interval(
        &self,
        field: &FieldRef,
        lower: std::ops::Bound<&Value>,
        upper: std::ops::Bound<&Value>,
        resume: Option<(Value, NodeKey)>,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(Value, NodeKey, &BodyKey, &ExtractedNode) -> bool,
    ) -> usize {
        if !self.packed.segments.is_empty() {
            let ranges = self.packed.field_ranges(
                field,
                FieldScanBounds::Interval { lower, upper },
                resume.as_ref(),
                &allows,
            );
            return self.packed.scan_field_ranges(
                ranges,
                &allows,
                usize::MAX,
                |value, key, source, node| {
                    self.snapshot
                        .body_key(source)
                        .is_some_and(|body| visit(value, key, body, node))
                },
            );
        }
        let Some(posting) = self
            .field_names
            .get(field)
            .and_then(|key| self.ordered_fields.get(&key))
        else {
            return 0;
        };
        let (mut lower, upper) = field_interval_bounds(field, lower, upper);
        if let Some((value, key)) = resume {
            let node = self.nodes.get(&key).copied().unwrap_or(NodeIx(0));
            lower = later_lower_bound(
                lower,
                std::ops::Bound::Included(FieldValue {
                    value: Arc::new(value),
                    key: Arc::new(key),
                    node,
                }),
            );
        }
        let mut visited = 0usize;
        visit_partitioned(
            Some(posting),
            (lower, upper),
            &allows,
            usize::MAX,
            |entry| {
                if self.row(entry.node).is_none() {
                    return true;
                }
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(entry.node) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(
                    entry.value.as_ref().clone(),
                    entry.key.as_ref().clone(),
                    source,
                    &node,
                )
            },
        );
        visited
    }

    /// Scan one Body's canonical row order from an inclusive row offset.
    pub fn scan_body(
        &self,
        body: &BodyKey,
        schema: &SchemaRef,
        resume_row: u32,
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
        mut visit: impl FnMut(u32, &BodyKey, &ExtractedNode) -> bool,
    ) -> usize {
        if !self.packed.segments.is_empty() {
            let Some(source) = self.snapshot.body_ix(body) else {
                return 0;
            };
            let Some(owner) = self
                .packed
                .bodies
                .get(source.as_u32() as usize)
                .and_then(|owner| *owner)
            else {
                return 0;
            };
            let Some(segment) = self.packed.segment(owner.segment) else {
                return 0;
            };
            let Some(body_row) = segment.body_rows.get(owner.body as usize) else {
                return 0;
            };
            let mut visited = 0usize;
            for (row, node) in
                packed_slice(&segment.body_nodes, body_row.node_start, body_row.node_len)
                    .iter()
                    .enumerate()
                    .skip(usize::try_from(resume_row).unwrap_or(usize::MAX))
            {
                let reference = PackedNodeRef {
                    segment: owner.segment,
                    node: *node,
                };
                let Some(key) = segment.node_key(*node) else {
                    continue;
                };
                if key.schema != *schema || !self.packed.is_live(reference) {
                    continue;
                }
                let Some(materialized) = self.packed.materialize(reference, &allows) else {
                    continue;
                };
                visited = visited.saturating_add(1);
                if !visit(u32::try_from(row).unwrap_or(u32::MAX), body, &materialized) {
                    break;
                }
            }
            return visited;
        }
        let Some(rows) = self.body_rows_for(body) else {
            return 0;
        };
        let mut visited = 0usize;
        for (row, node) in rows
            .nodes
            .iter()
            .copied()
            .enumerate()
            .skip(usize::try_from(resume_row).unwrap_or(usize::MAX))
        {
            let Some((_, column)) = self.row(node) else {
                continue;
            };
            if column.key.schema != *schema || !allows(column.gate.as_deref()) {
                continue;
            }
            let Some(materialized) = self.materialize_admitted(node, &allows) else {
                continue;
            };
            visited = visited.saturating_add(1);
            if !visit(u32::try_from(row).unwrap_or(u32::MAX), body, &materialized) {
                break;
            }
        }
        visited
    }

    fn visit_posting(
        &self,
        posting: Option<&PartitionedPosting>,
        limit: usize,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        visit: &mut impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        let mut visited = 0usize;
        let available = visit_partitioned(
            posting,
            (std::ops::Bound::Unbounded, std::ops::Bound::Unbounded),
            allows,
            limit,
            |index| {
                if self.row(index).is_none() {
                    return true;
                }
                let Some(node) = self.materialize_admitted(index, allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(index) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(source, &node)
            },
        );
        Visit { available, visited }
    }

    fn visit_ordered_nodes(
        &self,
        posting: Option<&PartitionedOrderedNodes>,
        limit: usize,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        visit: &mut impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        let mut visited = 0usize;
        let available = visit_partitioned(
            posting,
            (std::ops::Bound::Unbounded, std::ops::Bound::Unbounded),
            allows,
            limit,
            |entry| {
                if self.row(entry.node).is_none() {
                    return true;
                }
                let Some(node) = self.materialize_admitted(entry.node, allows) else {
                    return true;
                };
                let Some(source) = self.source_for_node(entry.node) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(source, &node)
            },
        );
        Visit { available, visited }
    }

    fn visit_ordered_field(
        &self,
        posting: Option<&PartitionedOrderedField>,
        bounds: Option<(std::ops::Bound<FieldValue>, std::ops::Bound<FieldValue>)>,
        limit: usize,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
        visit: &mut impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        let mut visited = 0usize;
        let bounded = bounds.is_some();
        let bounds = bounds.unwrap_or((std::ops::Bound::Unbounded, std::ops::Bound::Unbounded));
        let available = visit_partitioned(posting, bounds, allows, limit, |entry| {
            if self.row(entry.node).is_none() {
                return true;
            }
            let Some(node) = self.materialize_admitted(entry.node, allows) else {
                return true;
            };
            let Some(source) = self.source_for_node(entry.node) else {
                return true;
            };
            visited = visited.saturating_add(1);
            visit(source, &node)
        });
        Visit {
            available: if bounded { visited } else { available },
            visited,
        }
    }

    fn row(&self, index: NodeIx) -> Option<(&BodyRows, &NodeColumn)> {
        let column = self.node_rows.get(index.0 as usize)?.as_ref()?;
        let rows = self
            .body_rows
            .get(column.body.as_u32() as usize)?
            .as_ref()?;
        Some((rows, column))
    }

    fn source_for_node(&self, index: NodeIx) -> Option<&BodyKey> {
        let column = self.node_rows.get(index.0 as usize)?.as_ref()?;
        self.snapshot.body_key(column.body)
    }

    fn materialize(&self, index: NodeIx) -> Option<ExtractedNode> {
        self.materialize_admitted(index, &|_| true)
    }

    fn materialize_admitted(
        &self,
        index: NodeIx,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> Option<ExtractedNode> {
        let (_, column) = self.row(index)?;
        if !allows(column.gate.as_deref()) {
            return None;
        }
        let fields = column
            .fields
            .iter()
            .filter(|field| allows(field.gate.as_deref()))
            .map(|field| ExtractedField {
                reference: field.reference.as_ref().clone(),
                value: field.value.as_ref().clone(),
                gate: field.gate.as_ref().map(|gate| gate.as_ref().clone()),
                terms: field.terms.to_vec(),
            })
            .collect();
        let edges = column
            .edges
            .iter()
            .filter(|edge| allows(Some(edge.gate.as_ref())))
            .map(|edge| ExtractedEdge {
                reference: edge.reference.as_ref().clone(),
                gate: edge.gate.as_ref().clone(),
                targets: edge
                    .targets
                    .iter()
                    .map(|target| target.as_ref().clone())
                    .collect(),
            })
            .collect();
        let features = column
            .features
            .iter()
            .filter(|feature| allows(feature.gate.as_deref()))
            .map(|feature| ExtractedFeature {
                reference: feature.reference.as_ref().clone(),
                gate: feature.gate.as_ref().map(|gate| gate.as_ref().clone()),
                value: feature.value.clone(),
            })
            .collect();
        Some(ExtractedNode {
            key: column.key.as_ref().clone(),
            gate: column.gate.as_ref().map(|gate| gate.as_ref().clone()),
            fields,
            edges,
            features,
        })
    }
}

/// Physical upper price of one combined source-Body extraction.
fn shape_price(shapes: &[crate::find::ExtractionShape]) -> (u64, u64, u64) {
    let mut nodes = 0u64;
    let mut postings = 0u64;
    let mut variable = 0u64;
    let mut implementation_transient = 0u64;
    for shape in shapes {
        let shape_nodes = u64::from(shape.nodes_per_body);
        nodes = nodes.saturating_add(shape_nodes);
        postings = postings.saturating_add(shape.postings_per_body);
        variable = variable.saturating_add(shape.variable_bytes_per_body);
        implementation_transient = implementation_transient.max(shape.transient_bytes_per_body);
    }
    let retained = 40u64
        .saturating_add(nodes.saturating_mul(192))
        .saturating_add(postings.saturating_mul(112))
        .saturating_add(variable);
    let transient = implementation_transient
        .saturating_add(nodes.saturating_mul(192))
        .saturating_add(postings.saturating_mul(112))
        .saturating_add(variable);
    (retained, transient, postings)
}

/// Retained price for all Bodies at one exact source coordinate, using the
/// affine growth declaration over observed canonical source bytes. Per-Body
/// hard maxima remain the cap and are used for the single streamed transient.
fn growth_price(
    shapes: &[crate::find::ExtractionShape],
    bodies: u64,
    source_bytes: u64,
) -> (u64, u64, u64) {
    let source_kib_upper = source_bytes.saturating_add(bodies.saturating_mul(1023)) / 1024;
    let mut nodes = 0u64;
    let mut postings = 0u64;
    let mut variable = 0u64;
    for shape in shapes {
        nodes = nodes.saturating_add(
            bodies
                .saturating_mul(u64::from(shape.growth.base_nodes_per_body))
                .saturating_add(
                    source_kib_upper.saturating_mul(u64::from(shape.growth.nodes_per_source_kib)),
                )
                .min(bodies.saturating_mul(u64::from(shape.nodes_per_body))),
        );
        postings = postings.saturating_add(
            bodies
                .saturating_mul(shape.growth.base_postings_per_body)
                .saturating_add(
                    source_kib_upper.saturating_mul(shape.growth.postings_per_source_kib),
                )
                .min(bodies.saturating_mul(shape.postings_per_body)),
        );
        variable = variable.saturating_add(
            bodies
                .saturating_mul(shape.growth.base_variable_bytes_per_body)
                .saturating_add(
                    source_bytes.saturating_mul(shape.growth.variable_bytes_per_source_byte),
                )
                .min(bodies.saturating_mul(shape.variable_bytes_per_body)),
        );
    }
    let retained = bodies
        .saturating_mul(40)
        .saturating_add(nodes.saturating_mul(192))
        .saturating_add(postings.saturating_mul(112))
        .saturating_add(variable);
    let (_, transient, _) = shape_price(shapes);
    (retained, transient, postings)
}

#[cfg(test)]
pub(crate) fn snapshot_for_test(bodies: &[BodyExtraction]) -> Arc<replica::ReadSnapshot> {
    let fallback_schema = replica::body::SchemaId::parse("test.record").expect("test schema");
    let encoding = replica::body::EncodingId::parse("postcard").expect("test encoding");
    Arc::new(replica::ReadSnapshot::from_body_rows_for_test(
        bodies.iter().map(|extraction| {
            let schema = extraction
                .nodes
                .first()
                .map(|node| node.key.schema.name.clone())
                .unwrap_or_else(|| fallback_schema.clone());
            let key = fabric::Key::from_bytes(extraction.body.body.as_bytes().to_vec());
            let image =
                fabric::BodySnapshot::from_export(&key, fabric::BodyExport::Atomic(Vec::new()))
                    .expect("test Body image");
            (
                extraction.body.clone(),
                replica::body::BodyBinding {
                    schema,
                    schema_version: 1,
                    encoding: encoding.clone(),
                    mutation_model: replica::body::MUTATION_ATOMIC,
                },
                extraction.stamp.clone(),
                image,
            )
        }),
    ))
}

fn validate_extraction(body: &BodyExtraction, limits: Limits) -> Result<(), Failure> {
    if body.stamp.len() > limits.body_stamp_bytes {
        return Err(Failure::Limit("body stamp bytes"));
    }
    if body.nodes.len() > limits.nodes_per_body {
        return Err(Failure::Limit("nodes per body"));
    }
    let mut nodes = BTreeSet::new();
    for node in &body.nodes {
        if !nodes.insert(node.key.clone()) {
            return Err(Failure::DuplicateNode(node.key.clone()));
        }
        if node.fields.len() > limits.fields_per_node {
            return Err(Failure::Limit("fields per node"));
        }
        if node.edges.len() > limits.edges_per_node {
            return Err(Failure::Limit("edges per node"));
        }
        if node.features.len() > limits.features_per_node {
            return Err(Failure::Limit("features per node"));
        }
        if node
            .gate
            .as_ref()
            .is_some_and(|gate| gate.schema != node.key.schema)
        {
            return Err(Failure::Invalid("node gate schema"));
        }
        let mut fields = BTreeSet::new();
        for field in &node.fields {
            if field.reference.schema != node.key.schema {
                return Err(Failure::Invalid("field schema"));
            }
            if !fields.insert(field.reference.clone()) {
                return Err(Failure::Invalid("duplicate field"));
            }
            if field
                .gate
                .as_ref()
                .is_some_and(|gate| gate.schema != node.key.schema)
            {
                return Err(Failure::Invalid("field gate schema"));
            }
            if field.value.variable_len() > limits.value_bytes {
                return Err(Failure::Limit("field value bytes"));
            }
            if field.terms.len() > limits.terms_per_field {
                return Err(Failure::Limit("terms per field"));
            }
            let mut terms = BTreeSet::new();
            for term in &field.terms {
                if term.is_empty() {
                    return Err(Failure::Invalid("empty term"));
                }
                if term.len() > limits.term_bytes {
                    return Err(Failure::Limit("term bytes"));
                }
                if !terms.insert(term.as_ref()) {
                    return Err(Failure::Invalid("duplicate term"));
                }
            }
        }
        let mut edges = BTreeSet::new();
        for edge in &node.edges {
            if edge.reference.schema != node.key.schema || edge.gate.schema != node.key.schema {
                return Err(Failure::Invalid("edge schema"));
            }
            if !edges.insert(edge.reference.clone()) {
                return Err(Failure::Invalid("duplicate edge"));
            }
            if edge.targets.len() > limits.targets_per_edge {
                return Err(Failure::Limit("targets per edge"));
            }
            let mut targets = BTreeSet::new();
            for target in &edge.targets {
                if !targets.insert(target) {
                    return Err(Failure::Invalid("duplicate edge target"));
                }
            }
        }
        let mut features = BTreeSet::new();
        for feature in &node.features {
            if feature.reference.schema != node.key.schema {
                return Err(Failure::Invalid("feature schema"));
            }
            if feature
                .gate
                .as_ref()
                .is_some_and(|gate| gate.schema != node.key.schema)
            {
                return Err(Failure::Invalid("feature gate schema"));
            }
            if !features.insert(feature.reference.clone()) {
                return Err(Failure::Invalid("duplicate feature"));
            }
            if feature.value.len() > limits.feature_bytes {
                return Err(Failure::Limit("feature bytes"));
            }
        }
    }
    Ok(())
}

fn canonicalize_extraction(body: &mut BodyExtraction) {
    body.nodes.sort_by(|left, right| left.key.cmp(&right.key));
    for node in &mut body.nodes {
        node.fields
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        for field in &mut node.fields {
            field.terms.sort();
        }
        node.edges
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        for edge in &mut node.edges {
            edge.targets.sort();
        }
        node.features
            .sort_by(|left, right| left.reference.cmp(&right.reference));
    }
}

fn retained_node_bytes(node: &ExtractedNode) -> u64 {
    let mut bytes = usize_u64(node.key.node.as_bytes().len());
    for field in &node.fields {
        bytes = bytes
            .saturating_add(usize_u64(field.reference.name.as_bytes().len()))
            .saturating_add(usize_u64(field.value.variable_len()));
        for term in &field.terms {
            bytes = bytes.saturating_add(usize_u64(term.len()));
        }
    }
    for edge in &node.edges {
        bytes = bytes.saturating_add(usize_u64(edge.reference.name.as_bytes().len()));
        for target in &edge.targets {
            bytes = bytes.saturating_add(usize_u64(target.node.as_bytes().len()));
        }
    }
    for feature in &node.features {
        bytes = bytes
            .saturating_add(usize_u64(feature.reference.name.as_bytes().len()))
            .saturating_add(usize_u64(feature.value.len()));
    }
    bytes
}

fn usize_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn count_partitioned<T>(
    partitions: Option<&Partitioned<T>>,
    allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
) -> usize
where
    T: Clone + Ord,
{
    let Some(partitions) = partitions else {
        return 0;
    };
    partitions
        .iter()
        .filter(|(visibility, _)| visibility.admitted(allows))
        .map(|(_, posting)| posting.len())
        .sum()
}

/// Merge only admitted posting partitions in canonical entry order. Exact
/// availability is a metadata sum for an unbounded direct posting; range
/// callers deliberately treat the returned value as a visited lower bound.
fn visit_partitioned<T>(
    partitions: Option<&Partitioned<T>>,
    bounds: (std::ops::Bound<T>, std::ops::Bound<T>),
    allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    limit: usize,
    mut visit: impl FnMut(T) -> bool,
) -> usize
where
    T: Clone + Ord,
{
    let Some(partitions) = partitions else {
        return 0;
    };
    let available = partitions
        .iter()
        .filter(|(visibility, _)| visibility.admitted(allows))
        .map(|(_, posting)| posting.len())
        .sum();
    let mut iterators = partitions
        .iter()
        .filter(|(visibility, _)| visibility.admitted(allows))
        .map(|(_, posting)| posting.range(bounds.clone()))
        .collect::<Vec<_>>();
    let mut pending = BinaryHeap::new();
    for (partition, iterator) in iterators.iter_mut().enumerate() {
        if let Some(entry) = iterator.next() {
            pending.push(Reverse((entry.clone(), partition)));
        }
    }
    let mut visited = 0usize;
    while visited < limit {
        let Some(Reverse((entry, partition))) = pending.pop() else {
            break;
        };
        visited = visited.saturating_add(1);
        if !visit(entry) {
            break;
        }
        if let Some(next) = iterators[partition].next() {
            pending.push(Reverse((next.clone(), partition)));
        }
    }
    available
}

fn partitioned_insert<K, T>(
    index: &mut PersistentMap<K, Partitioned<T>>,
    posting_key: K,
    visibility: Visibility,
    entry: T,
    work: &mut BuildWork,
) where
    K: Clone + Eq + Hash,
    T: Clone + Ord,
{
    let partitions = index.entry(posting_key).or_default();
    let posting = partitions.posting_mut(visibility);
    if posting.insert(entry).is_none() {
        work.postings_inserted = work.postings_inserted.saturating_add(1);
    }
}

fn partition_insert<T: Clone + Ord>(
    partitions: &mut Partitioned<T>,
    visibility: Visibility,
    entry: T,
    work: &mut BuildWork,
) {
    if partitions.posting_mut(visibility).insert(entry).is_none() {
        work.postings_inserted = work.postings_inserted.saturating_add(1);
    }
}

fn partition_remove<T: Clone + Ord>(
    partitions: &mut Partitioned<T>,
    visibility: &Visibility,
    entry: &T,
    work: &mut BuildWork,
) {
    if partitions.remove(visibility, entry) {
        work.postings_removed = work.postings_removed.saturating_add(1);
    }
}

fn partitioned_remove<K, T>(
    index: &mut PersistentMap<K, Partitioned<T>>,
    posting_key: &K,
    visibility: &Visibility,
    entry: &T,
    work: &mut BuildWork,
) where
    K: Clone + Eq + Hash,
    T: Clone + Ord,
{
    let Some(partitions) = index.get_mut(posting_key) else {
        return;
    };
    if !partitions.remove(visibility, entry) {
        return;
    }
    if partitions.is_empty() {
        index.remove(posting_key);
    }
    work.postings_removed = work.postings_removed.saturating_add(1);
}

fn field_value_bounds(
    field: &FieldRef,
    test: Test,
    value: &Value,
) -> (std::ops::Bound<FieldValue>, std::ops::Bound<FieldValue>) {
    use std::ops::Bound::{Excluded, Included, Unbounded};

    let entry = |value: Value, high: bool| FieldValue {
        value: Arc::new(value),
        key: Arc::new(NodeKey {
            schema: field.schema.clone(),
            node: crate::find::NodeId::new(if high {
                vec![u8::MAX; crate::find::MAX_NODE_ID_BYTES]
            } else {
                vec![0]
            })
            .expect("bounded sentinel node"),
        }),
        node: if high { NodeIx(u32::MAX) } else { NodeIx(0) },
    };
    let (kind_start, kind_end) = value_kind_bounds(value);
    let lower_kind = Included(entry(kind_start, false));
    let upper_kind = kind_end
        .map(|value| Excluded(entry(value, false)))
        .unwrap_or(Unbounded);
    match test {
        Test::Equal => (
            Included(entry(value.clone(), false)),
            Included(entry(value.clone(), true)),
        ),
        Test::Less => (lower_kind, Excluded(entry(value.clone(), false))),
        Test::LessOrEqual => (lower_kind, Included(entry(value.clone(), true))),
        Test::Greater => (Excluded(entry(value.clone(), true)), upper_kind),
        Test::GreaterOrEqual => (Included(entry(value.clone(), false)), upper_kind),
        Test::Contains => (lower_kind, upper_kind),
        Test::Prefix => {
            let upper = next_prefix(value)
                .map(|value| Excluded(entry(value, false)))
                .unwrap_or(upper_kind);
            (Included(entry(value.clone(), false)), upper)
        }
    }
}

fn field_interval_bounds(
    field: &FieldRef,
    lower: std::ops::Bound<&Value>,
    upper: std::ops::Bound<&Value>,
) -> (std::ops::Bound<FieldValue>, std::ops::Bound<FieldValue>) {
    use std::ops::Bound::{Excluded, Included, Unbounded};

    let entry = |value: &Value, high: bool| FieldValue {
        value: Arc::new(value.clone()),
        key: Arc::new(NodeKey {
            schema: field.schema.clone(),
            node: crate::find::NodeId::new(if high {
                vec![u8::MAX; crate::find::MAX_NODE_ID_BYTES]
            } else {
                vec![0]
            })
            .expect("bounded sentinel node"),
        }),
        node: if high { NodeIx(u32::MAX) } else { NodeIx(0) },
    };
    let lower = match lower {
        Included(value) => Included(entry(value, false)),
        Excluded(value) => Excluded(entry(value, true)),
        Unbounded => Unbounded,
    };
    let upper = match upper {
        Included(value) => Included(entry(value, true)),
        Excluded(value) => Excluded(entry(value, false)),
        Unbounded => Unbounded,
    };
    (lower, upper)
}

fn later_lower_bound(
    left: std::ops::Bound<FieldValue>,
    right: std::ops::Bound<FieldValue>,
) -> std::ops::Bound<FieldValue> {
    use std::cmp::Ordering;
    use std::ops::Bound::{Excluded, Included, Unbounded};

    match (&left, &right) {
        (Unbounded, _) => right,
        (_, Unbounded) => left,
        (
            Included(left_value) | Excluded(left_value),
            Included(right_value) | Excluded(right_value),
        ) => match left_value.cmp(right_value) {
            Ordering::Less => right,
            Ordering::Greater => left,
            Ordering::Equal => {
                if matches!(left, Excluded(_)) || matches!(right, Excluded(_)) {
                    Excluded(left_value.clone())
                } else {
                    Included(left_value.clone())
                }
            }
        },
    }
}

fn value_kind_bounds(value: &Value) -> (Value, Option<Value>) {
    match value {
        Value::Bool(_) => (Value::Bool(false), Some(Value::Signed(i64::MIN))),
        Value::Signed(_) => (Value::Signed(i64::MIN), Some(Value::Unsigned(0))),
        Value::Unsigned(_) => (Value::Unsigned(0), Some(Value::bytes(Vec::new()))),
        Value::Bytes(_) => (Value::bytes(Vec::new()), Some(Value::text(String::new()))),
        Value::Text(_) => (Value::text(String::new()), None),
    }
}

fn next_prefix(value: &Value) -> Option<Value> {
    match value {
        Value::Bytes(prefix) => {
            let mut next = prefix.to_vec();
            while let Some(last) = next.pop() {
                if last != u8::MAX {
                    next.push(last.saturating_add(1));
                    return Some(Value::bytes(next));
                }
            }
            None
        }
        Value::Text(prefix) => {
            let mut chars = prefix.chars().collect::<Vec<_>>();
            while let Some(last) = chars.pop() {
                let mut scalar = u32::from(last).saturating_add(1);
                while scalar <= u32::from(char::MAX) {
                    if let Some(next) = char::from_u32(scalar) {
                        chars.push(next);
                        return Some(Value::text(chars.into_iter().collect::<String>()));
                    }
                    scalar = scalar.saturating_add(1);
                }
            }
            None
        }
        Value::Bool(_) | Value::Signed(_) | Value::Unsigned(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        find::{
            Atom, Bound, EdgeRef, ExtractedEdge, ExtractedField, ExtractionGrowth, ExtractionShape,
            Extractor, FieldRef, GateRef, Mode, NodeId, Op, Policy, Predicate, Query, SchemaRef,
            Seek, SourceRef, Step, StepId, Test, EXTRACTOR_ABI_VERSION,
        },
        publication::{ExtractorSchemaDigest, MaterializationId, PublicationId},
    };
    use replica::body::{BodyId, EncodingId, SchemaId, WorldId};

    fn coordinate(root: u8, materialization: u64) -> WorldPublicationId {
        WorldPublicationId::new(
            PublicationId::new(
                [root; 32],
                [2; 32],
                ExtractorSchemaDigest::from_digest([3; 32]),
            ),
            MaterializationId::from_u64(materialization).expect("nonzero materialization"),
        )
    }

    fn schema() -> SchemaRef {
        SchemaRef {
            name: SchemaId::parse("issues.issue").expect("schema"),
            version: 1,
        }
    }

    fn body(number: u8) -> BodyKey {
        BodyKey::new(
            WorldId::parse("dev.lait.issues").expect("world"),
            BodyId::from_bytes([number; 16]),
        )
    }

    fn node(number: u8, title: &str) -> ExtractedNode {
        let schema = schema();
        let title_ref = FieldRef {
            schema: schema.clone(),
            name: SchemaId::parse("title").expect("field"),
        };
        ExtractedNode {
            key: NodeKey {
                schema: schema.clone(),
                node: NodeId::new(vec![number]).expect("node"),
            },
            gate: Some(GateRef {
                schema,
                name: SchemaId::parse("read").expect("gate"),
            }),
            fields: vec![ExtractedField {
                reference: title_ref,
                value: Value::text(title),
                gate: None,
                terms: vec![Arc::from(title.as_bytes())],
            }],
            edges: Vec::new(),
            features: Vec::new(),
        }
    }

    fn extraction(number: u8, title: &str) -> BodyExtraction {
        BodyExtraction {
            body: body(number),
            stamp: vec![number],
            nodes: vec![node(number, title)],
        }
    }

    fn build_test(
        coordinate: WorldPublicationId,
        limits: Limits,
        bodies: Vec<BodyExtraction>,
    ) -> Result<(Corpus, BuildWork), Failure> {
        let snapshot = snapshot_for_test(&bodies);
        Corpus::build(coordinate, limits, snapshot, bodies)
    }

    fn test_extractor(shape: ExtractionShape) -> Extractor {
        Extractor {
            schema: schema(),
            source: SourceRef {
                name: schema().name,
                version: 1,
            },
            abi_version: EXTRACTOR_ABI_VERSION,
            semantic_digest: [9; 32],
            shape,
        }
    }

    #[test]
    fn build_estimates_use_exact_source_counts_and_delta_scope() {
        let bodies = vec![extraction(1, "one"), extraction(2, "two")];
        let snapshot = snapshot_for_test(&bodies);
        let extractor = test_extractor(ExtractionShape::new(1, 4, 4, 32, 32, 4_096).with_growth(
            ExtractionGrowth {
                base_nodes_per_body: 1,
                nodes_per_source_kib: 0,
                base_postings_per_body: 2,
                postings_per_source_kib: 1,
                base_variable_bytes_per_body: 8,
                variable_bytes_per_source_byte: 1,
            },
        ));
        let world = WorldId::parse("dev.lait.issues").expect("world");
        let full = Corpus::estimate_build_bytes(&snapshot, &world, &[extractor.clone()]);
        let (expected, body_transient, _) = growth_price(&[extractor.shape], 2, 0);
        assert_eq!(full.retained_bytes, expected);
        assert_eq!(full.transient_bytes, body_transient + 8 * 1024 * 1024);
        let historical = replica::GenerationFootprint {
            body_count: 2,
            snapshot_retained_bytes: snapshot.retained_bytes_estimate(),
            reconstruction_depth: 1,
            reconstruction_delta_bytes: 1,
            reconstruction_transient_bytes: 1,
            sources: vec![replica::GenerationSourceFootprint {
                world: world.clone(),
                schema: extractor.source.name.clone(),
                version: extractor.source.version,
                body_count: 2,
                payload_bytes: 0,
            }],
        };
        assert_eq!(
            Corpus::estimate_build_bytes_from_footprint(
                &historical,
                &world,
                std::slice::from_ref(&extractor),
            ),
            full,
            "historical admission uses the same source-growth price without a snapshot"
        );

        let (corpus, _) = Corpus::build(
            coordinate(1, 1),
            Limits::default(),
            snapshot.clone(),
            bodies,
        )
        .expect("corpus");
        let changed = vec![body(1), body(1)];
        let delta = corpus.estimate_delta_build_bytes(&snapshot, &world, &[extractor], &changed);
        assert!(delta.retained_bytes > expected / 2);
        assert!(delta.retained_bytes < full.retained_bytes + 1024 * 1024);
        assert_eq!(delta.transient_bytes, body_transient + 2 * 1024 * 1024);
    }

    #[test]
    fn full_build_indexes_schema_exact_value_and_term() {
        let (corpus, work) = build_test(
            coordinate(1, 1),
            Limits::default(),
            vec![extraction(1, "alpha"), extraction(2, "beta")],
        )
        .expect("build");
        assert_eq!(corpus.body_count(), 2);
        assert_eq!(corpus.node_count(), 2);
        assert_eq!(work.nodes_inserted, 2);

        let mut titles = Vec::new();
        let visit = corpus.visit_schema(
            &schema(),
            1,
            |_| true,
            |_, node| {
                titles.push(node.fields[0].value.clone());
                true
            },
        );
        assert_eq!(visit.available, 2);
        assert_eq!(visit.visited, 1, "the explicit visit limit is charged");

        let title = FieldRef {
            schema: schema(),
            name: SchemaId::parse("title").expect("field"),
        };
        let exact = corpus.visit_exact(
            &title,
            &Value::text("alpha"),
            4,
            |_| true,
            |source, _| {
                assert_eq!(source, &body(1));
                true
            },
        );
        assert_eq!(
            exact,
            Visit {
                available: 1,
                visited: 1
            }
        );
        let term = corpus.visit_term(
            &title,
            b"beta",
            false,
            4,
            |_| true,
            |source, _, frequency| {
                assert_eq!(source, &body(2));
                assert_eq!(frequency, 1);
                true
            },
        );
        assert_eq!(
            term,
            Visit {
                available: 1,
                visited: 1
            }
        );
        let prefix = corpus.visit_term(&title, b"al", true, 4, |_| true, |_, _, _| true);
        assert_eq!(
            prefix,
            Visit {
                available: 0,
                visited: 0
            },
            "analyzed Prefix is refused at validation and never materialized"
        );
        assert_eq!(
            corpus.visit_term(&title, b"al", false, 4, |_| true, |_, _, _| true),
            Visit {
                available: 0,
                visited: 0
            },
            "Token al must not alias Prefix al"
        );
    }

    #[test]
    fn delta_replaces_only_named_body_and_removes_old_postings() {
        let (corpus, _) = build_test(
            coordinate(1, 1),
            Limits::default(),
            vec![extraction(1, "alpha"), extraction(2, "beta")],
        )
        .expect("build");
        let unchanged_ref = corpus
            .packed
            .node_ref(&node(2, "beta").key)
            .expect("node slot");
        let unchanged = corpus
            .packed
            .segment(unchanged_ref.segment)
            .expect("node segment")
            .clone();

        let (next, work) = corpus
            .apply(CorpusDelta {
                base: coordinate(1, 1),
                next: coordinate(2, 2),
                snapshot: corpus.snapshot.clone(),
                bodies: vec![extraction(1, "gamma")],
            })
            .expect("delta");
        assert_eq!(next.node_count(), 2);
        assert_eq!(work.nodes_removed, 1);
        assert_eq!(work.nodes_inserted, 1);
        let still_ref = next
            .packed
            .node_ref(&node(2, "beta").key)
            .expect("shared node row");
        let still_shared = next
            .packed
            .segment(still_ref.segment)
            .expect("shared segment");
        assert!(Arc::ptr_eq(&unchanged, still_shared));

        let title = FieldRef {
            schema: schema(),
            name: SchemaId::parse("title").expect("field"),
        };
        assert_eq!(
            next.visit_exact(&title, &Value::text("alpha"), 1, |_| true, |_, _| true),
            Visit {
                available: 0,
                visited: 0
            }
        );
        assert_eq!(
            next.visit_exact(&title, &Value::text("gamma"), 1, |_| true, |_, _| true),
            Visit {
                available: 1,
                visited: 1
            }
        );
    }

    #[test]
    fn delete_only_retracts_rows_before_switching_body_dictionary() {
        let original = extraction(1, "alpha");
        let (corpus, _) =
            build_test(coordinate(1, 1), Limits::default(), vec![original.clone()]).expect("build");
        let tombstone = BodyExtraction {
            body: original.body,
            stamp: Vec::new(),
            nodes: Vec::new(),
        };
        let (next, work) = corpus
            .apply(CorpusDelta {
                base: coordinate(1, 1),
                next: coordinate(2, 2),
                snapshot: snapshot_for_test(&[]),
                bodies: vec![tombstone],
            })
            .expect("delete");
        assert_eq!(work.nodes_removed, 1);
        assert_eq!(next.body_count(), 0);
        assert_eq!(next.node_count(), 0);
        assert!(next.node(&node(1, "alpha").key).is_none());
    }

    #[test]
    fn delete_then_reused_body_slot_has_only_new_facts_and_shares_unchanged_rows() {
        let removed = extraction(1, "alpha");
        let unchanged = extraction(3, "steady");
        let (corpus, _) = build_test(
            coordinate(1, 1),
            Limits::default(),
            vec![removed.clone(), unchanged.clone()],
        )
        .expect("build");
        let held_ref = corpus
            .packed
            .node_ref(&unchanged.nodes[0].key)
            .expect("unchanged node");
        let held = corpus
            .packed
            .segment(held_ref.segment)
            .expect("unchanged segment")
            .clone();
        let inserted = extraction(2, "beta");
        let next_snapshot = snapshot_for_test(&[inserted.clone(), unchanged.clone()]);
        let (next, _) = corpus
            .apply(CorpusDelta {
                base: coordinate(1, 1),
                next: coordinate(2, 2),
                snapshot: next_snapshot,
                bodies: vec![
                    BodyExtraction {
                        body: removed.body,
                        stamp: Vec::new(),
                        nodes: Vec::new(),
                    },
                    inserted.clone(),
                ],
            })
            .expect("delete + insert");
        assert!(next.node(&node(1, "alpha").key).is_none());
        assert!(next.node(&inserted.nodes[0].key).is_some());
        let still_ref = next
            .packed
            .node_ref(&unchanged.nodes[0].key)
            .expect("unchanged row retained");
        let still = next
            .packed
            .segment(still_ref.segment)
            .expect("unchanged segment retained");
        assert!(Arc::ptr_eq(&held, still));
    }

    #[test]
    fn coordinate_mismatch_and_duplicate_nodes_fail_without_mutating_base() {
        let (corpus, _) = build_test(
            coordinate(1, 1),
            Limits::default(),
            vec![extraction(1, "alpha")],
        )
        .expect("build");
        let mismatch = corpus.apply(CorpusDelta {
            base: coordinate(9, 9),
            next: coordinate(2, 2),
            snapshot: corpus.snapshot.clone(),
            bodies: Vec::new(),
        });
        assert!(matches!(mismatch, Err(Failure::CoordinateMismatch { .. })));

        let duplicate = BodyExtraction {
            body: body(2),
            stamp: vec![2],
            nodes: vec![node(1, "collision")],
        };
        assert!(matches!(
            corpus.apply(CorpusDelta {
                base: coordinate(1, 1),
                next: coordinate(2, 2),
                snapshot: snapshot_for_test(&[extraction(1, "alpha"), duplicate.clone()]),
                bodies: vec![duplicate],
            }),
            Err(Failure::DuplicateNode(_))
        ));
        assert_eq!(corpus.coordinate(), coordinate(1, 1));
        assert_eq!(corpus.node_count(), 1);
    }

    #[test]
    fn empty_delta_moves_coordinate_and_shares_body_columns() {
        let (corpus, _) = build_test(
            coordinate(1, 1),
            Limits::default(),
            vec![extraction(1, "alpha")],
        )
        .expect("build");
        let (next, work) = corpus
            .apply(CorpusDelta {
                base: coordinate(1, 1),
                next: coordinate(2, 2),
                snapshot: corpus.snapshot.clone(),
                bodies: Vec::new(),
            })
            .expect("coordinate-only delta");
        let reference = corpus.packed.node_ref(&node(1, "alpha").key).expect("node");
        let prior_segment = corpus
            .packed
            .segment(reference.segment)
            .expect("prior segment");
        let next_segment = next
            .packed
            .segment(reference.segment)
            .expect("next segment");
        assert!(Arc::ptr_eq(prior_segment, next_segment));
        assert_eq!(work.nodes_inserted, 0);
        assert_eq!(next.coordinate(), coordinate(2, 2));
    }

    #[test]
    fn bounds_are_checked_before_any_replacement() {
        let limits = Limits {
            value_bytes: 3,
            ..Limits::default()
        };
        let result = build_test(coordinate(1, 1), limits, vec![extraction(1, "large")]);
        assert!(matches!(result, Err(Failure::Limit("field value bytes"))));
    }

    #[test]
    fn ordered_posting_removes_and_reuses_dense_identity() {
        let mut posting = PersistentSet::new();
        posting.insert(NodeIx(91));
        posting.insert(NodeIx(3));
        posting.insert(NodeIx(40));
        assert_eq!(
            posting.iter().copied().collect::<Vec<_>>(),
            vec![NodeIx(3), NodeIx(40), NodeIx(91)]
        );
        assert_eq!(posting.remove(&NodeIx(40)), Some(NodeIx(40)));
        assert_eq!(
            posting.iter().copied().collect::<Vec<_>>(),
            vec![NodeIx(3), NodeIx(91)]
        );
    }

    #[test]
    fn link_targets_share_the_interned_node_identity() {
        let total = 4;
        let (corpus, _) = build_test(
            coordinate(1, 1),
            Limits::default(),
            scale_extractions(total),
        )
        .expect("build");
        let source = NodeKey {
            schema: schema(),
            node: NodeId::new(0u32.to_be_bytes().to_vec()).expect("source"),
        };
        let target = NodeKey {
            schema: schema(),
            node: NodeId::new(1u32.to_be_bytes().to_vec()).expect("target"),
        };
        let source_ref = corpus.packed.node_ref(&source).expect("source node");
        let target_ref = corpus.packed.node_ref(&target).expect("target node");
        assert_eq!(source_ref.segment, target_ref.segment);
        let segment = corpus
            .packed
            .segment(source_ref.segment)
            .expect("packed segment");
        let source_row = &segment.nodes[source_ref.node as usize];
        let edge = &packed_slice(
            &segment.edges,
            source_row.edge_start,
            u32::from(source_row.edge_len),
        )[0];
        let stored_target = packed_slice(&segment.targets, edge.target_start, edge.target_len)[0];
        assert_eq!(
            stored_target, segment.nodes[target_ref.node as usize].key,
            "edge target and live target node share one segment-local NodeKey id"
        );
    }

    fn scale_body(number: u64) -> BodyKey {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&number.to_be_bytes());
        BodyKey::new(
            WorldId::parse("dev.lait.issues").expect("world"),
            BodyId::from_bytes(bytes),
        )
    }

    fn scale_node(number: u32, total: u32) -> ExtractedNode {
        let schema = schema();
        let key = NodeKey {
            schema: schema.clone(),
            node: NodeId::new(number.to_be_bytes().to_vec()).expect("node"),
        };
        let target = NodeKey {
            schema: schema.clone(),
            node: NodeId::new(
                number
                    .wrapping_add(1)
                    .wrapping_rem(total)
                    .to_be_bytes()
                    .to_vec(),
            )
            .expect("target"),
        };
        ExtractedNode {
            key,
            gate: Some(GateRef {
                schema: schema.clone(),
                name: SchemaId::parse("read").expect("gate"),
            }),
            fields: vec![ExtractedField {
                reference: FieldRef {
                    schema: schema.clone(),
                    name: SchemaId::parse("title").expect("field"),
                },
                value: Value::text("shared"),
                gate: None,
                terms: vec![Arc::from(&b"shared"[..])],
            }],
            edges: vec![ExtractedEdge {
                reference: EdgeRef {
                    schema: schema.clone(),
                    name: SchemaId::parse("links").expect("edge"),
                },
                gate: GateRef {
                    schema,
                    name: SchemaId::parse("read").expect("gate"),
                },
                targets: vec![target],
            }],
            features: Vec::new(),
        }
    }

    fn scale_extractions(total: u32) -> Vec<BodyExtraction> {
        const NODES_PER_BODY: u32 = 256;
        let mut bodies = Vec::new();
        for first in (0..total).step_by(NODES_PER_BODY as usize) {
            let end = first.saturating_add(NODES_PER_BODY).min(total);
            bodies.push(BodyExtraction {
                body: scale_body(u64::from(first / NODES_PER_BODY)),
                stamp: first.to_be_bytes().to_vec(),
                nodes: (first..end)
                    .map(|number| scale_node(number, total))
                    .collect(),
            });
        }
        bodies
    }

    fn scale_record_extractions(total: u32) -> Vec<BodyExtraction> {
        (0..total)
            .map(|number| scale_record_extraction(number, total))
            .collect()
    }

    fn scale_record_extraction(number: u32, total: u32) -> BodyExtraction {
        BodyExtraction {
            body: scale_body(u64::from(number)),
            stamp: number.to_be_bytes().to_vec(),
            nodes: vec![scale_node(number, total)],
        }
    }

    fn v4_node(
        body_number: u32,
        ordinal: u16,
        field_count: u8,
        term_count: u16,
        edge_count: u8,
    ) -> ExtractedNode {
        let schema = schema();
        let node_number = (u64::from(body_number) << 16) | u64::from(ordinal);
        let key = NodeKey {
            schema: schema.clone(),
            node: NodeId::new(node_number.to_be_bytes().to_vec()).expect("v4 node"),
        };
        let mut fields = Vec::with_capacity(usize::from(field_count));
        for field in 0..field_count {
            let name = SchemaId::parse(&format!("scalar_{field}")).expect("v4 field");
            let terms = if field == 0 {
                (0..term_count)
                    .map(|term| {
                        Arc::<[u8]>::from(
                            format!("term_{term}_{}", body_number % 10_000).into_bytes(),
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            fields.push(ExtractedField {
                reference: FieldRef {
                    schema: schema.clone(),
                    name,
                },
                value: if field == 0 && term_count != 0 {
                    Value::text("search")
                } else {
                    Value::Unsigned((node_number << 8) | u64::from(field))
                },
                gate: None,
                terms,
            });
        }
        let edges = (0..edge_count)
            .map(|edge| ExtractedEdge {
                reference: EdgeRef {
                    schema: schema.clone(),
                    name: SchemaId::parse(&format!("relation_{edge}")).expect("v4 edge"),
                },
                gate: GateRef {
                    schema: schema.clone(),
                    name: SchemaId::parse("read").expect("gate"),
                },
                targets: vec![NodeKey {
                    schema: schema.clone(),
                    node: NodeId::new(
                        node_number
                            .wrapping_add(u64::from(edge))
                            .wrapping_add(1)
                            .to_be_bytes()
                            .to_vec(),
                    )
                    .expect("v4 target"),
                }],
            })
            .collect();
        ExtractedNode {
            key,
            gate: Some(GateRef {
                schema,
                name: SchemaId::parse("read").expect("gate"),
            }),
            fields,
            edges,
            features: Vec::new(),
        }
    }

    fn issues_v4_record_extraction(number: u32, _total: u32) -> BodyExtraction {
        let family = number % 100;
        let mut nodes = Vec::new();
        match family {
            0..=34 => nodes.push(v4_node(number, 0, 6, 0, 2)),
            35..=54 => {
                nodes.push(v4_node(number, 0, 8, 32, 0));
                nodes.push(v4_node(number, 1, 3, 0, 2));
                nodes.push(v4_node(number, 2, 3, 0, 2));
            }
            55..=69 => {
                nodes.push(v4_node(number, 0, 4, 5, 0));
                nodes.push(v4_node(number, 1, 3, 0, 2));
                nodes.push(v4_node(number, 2, 3, 0, 2));
            }
            70..=79 => {
                for ordinal in 0..3 {
                    nodes.push(v4_node(number, ordinal, 3, 0, 2));
                }
            }
            80..=89 => {
                nodes.push(v4_node(number, 0, 5, 0, 0));
                if number % 2 == 0 {
                    nodes.push(v4_node(number, 1, 5, 0, 0));
                }
            }
            90..=94 => {
                nodes.push(v4_node(number, 0, 6, 5, 0));
                nodes.push(v4_node(number, 1, 6, 0, 1));
            }
            _ => {
                nodes.push(v4_node(number, 0, 8, 20, 0));
                for ordinal in 1..8 {
                    nodes.push(v4_node(number, ordinal, 3, 0, 2));
                }
            }
        }
        BodyExtraction {
            body: scale_body(u64::from(number)),
            stamp: number.to_be_bytes().to_vec(),
            nodes,
        }
    }

    fn empty_payload(_: u32) -> usize {
        0
    }

    fn issues_v4_payload(number: u32) -> usize {
        match number % 100 {
            0..=34 => 350,
            35..=54 => 1_024,
            55..=69 => 450,
            70..=79 => 600,
            80..=89 => 300,
            90..=94 => 800,
            _ => 1_500,
        }
    }

    #[cfg(windows)]
    fn resident_bytes() -> usize {
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_non_paged_pool_usage: usize,
            quota_non_paged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut core::ffi::c_void;
            fn K32GetProcessMemoryInfo(
                process: *mut core::ffi::c_void,
                counters: *mut Counters,
                size: u32,
            ) -> i32;
        }
        let mut counters = Counters {
            cb: u32::try_from(std::mem::size_of::<Counters>()).expect("counter size"),
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };
        // SAFETY: `counters` is the exact Windows PROCESS_MEMORY_COUNTERS
        // layout, initialized with its byte size, and remains live and writable
        // for the duration of the call. GetCurrentProcess returns a pseudo-
        // handle valid in this process and requiring no close.
        let ok =
            unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &raw mut counters, counters.cb) };
        if ok == 0 {
            0
        } else {
            counters.working_set_size
        }
    }

    #[cfg(not(windows))]
    fn resident_bytes() -> usize {
        0
    }

    fn cold_scale_material(number: u32, plaintext_size: usize) -> fabric::Material {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lait/corpus-scale/cold-material/1\0");
        hasher.update(&number.to_be_bytes());
        let hash = *hasher.finalize().as_bytes();
        fabric::Material {
            format_version: fabric::CAUSAL_FORMAT_VERSION,
            checkpoint: fabric::ArtifactRef {
                hash,
                len: u64::try_from(plaintext_size)
                    .unwrap_or(u64::MAX)
                    .saturating_add(128),
                epoch: [0x5a; 16],
            },
            delta_tail: Vec::new(),
            history_root: None,
            history_count: 0,
            version: fabric::Version::empty(),
            plaintext_size: u64::try_from(plaintext_size).unwrap_or(u64::MAX),
        }
    }

    fn release_scale(
        total: u32,
        layout: &str,
        extractions: fn(u32) -> Vec<BodyExtraction>,
        record_extraction: fn(u32, u32) -> BodyExtraction,
        payload_size: fn(u32) -> usize,
        streaming: bool,
    ) {
        const MAX_LOGICAL_BYTES_PER_NODE: u64 = 1_024;
        const MAX_CORPUS_RSS_BYTES_PER_NODE: usize = 4 * 1024;
        const MAX_LOOKUP_100K: std::time::Duration = std::time::Duration::from_secs(5);
        let before_rows = resident_bytes();
        let hot_key = fabric::Key::from_bytes(b"issues-v4-hot-writer".to_vec());
        let mut hot_engine = fabric::Engine::new();
        hot_engine
            .commit(fabric::Transaction::new(
                "hot-issue",
                vec![
                    fabric::Op::CreateBody {
                        key: hot_key.clone(),
                    },
                    fabric::Op::RegisterSet {
                        key: hot_key.clone(),
                        path: "title".to_owned(),
                        value: b"one deliberately hot writer".to_vec(),
                    },
                ],
            ))
            .expect("hot writer fixture");
        let after_engine = resident_bytes();
        let rows_started = std::time::Instant::now();
        let bodies = (!streaming).then(|| extractions(total));
        let mut mutable_replica = None;
        let mut after_replica = after_engine;
        let snapshot = if let Some(bodies) = bodies.as_ref() {
            snapshot_for_test(bodies)
        } else {
            let binding = replica::body::BodyBinding {
                schema: SchemaId::parse("issues.issue").expect("schema"),
                schema_version: 1,
                encoding: EncodingId::parse("postcard").expect("encoding"),
                mutation_model: replica::body::MUTATION_ATOMIC,
            };
            let mut replica =
                replica::Replica::from_cold_body_records_for_scale((0..total).map(|number| {
                    let body_key = scale_body(u64::from(number));
                    (
                        body_key,
                        binding.clone(),
                        cold_scale_material(number, payload_size(number)),
                    )
                }));
            if layout == "issues-v4-mix" {
                replica.add_issues_v4_operational_metadata_for_scale();
            }
            after_replica = resident_bytes();
            let snapshot = Arc::new(replica.cold_read_snapshot_for_scale());
            mutable_replica = Some(replica);
            snapshot
        };
        let rows_elapsed = rows_started.elapsed();
        let before_build = resident_bytes();
        let build_started = std::time::Instant::now();
        let mut peak_build = before_build;
        let (corpus, work) = if streaming {
            let mut builder = CorpusBuilder::new(coordinate(1, 1), Limits::default(), snapshot);
            for number in 0..total {
                builder
                    .push(record_extraction(number, total))
                    .expect("streamed record extraction");
                if number % 4096 == 0 {
                    peak_build = peak_build.max(resident_bytes());
                }
            }
            builder.finish().expect("finish streamed record corpus")
        } else {
            Corpus::build(
                coordinate(1, 1),
                Limits::default(),
                snapshot,
                bodies.expect("materialized extraction fixture"),
            )
            .expect("release corpus build")
        };
        let build_elapsed = build_started.elapsed();
        let after_build = resident_bytes();
        assert!(hot_engine.body_snapshot(&hot_key).unwrap().is_some());
        if let Some(replica) = &mutable_replica {
            assert_eq!(replica.body_count(), u64::from(total));
        }
        peak_build = peak_build.max(after_build);
        let mutable_replica_rss = after_replica.saturating_sub(after_engine);
        let snapshot_rss = before_build.saturating_sub(after_replica);
        let corpus_rss = after_build.saturating_sub(before_build);
        let combined_steady_rss = after_build.saturating_sub(before_rows);
        let combined_peak_rss = peak_build.saturating_sub(before_rows);
        let snapshot_retained_estimate = corpus.snapshot.retained_bytes_estimate();
        let corpus_retained_estimate = corpus.retained_bytes_estimate();
        let publication_retained_estimate =
            snapshot_retained_estimate.saturating_add(corpus_retained_estimate);
        let mutable_replica_retained_estimate = mutable_replica
            .as_ref()
            .map_or(0, replica::Replica::mutable_retained_bytes_estimate);
        let (receipt_count, declared_content_count, unique_content_count) =
            mutable_replica.as_ref().map_or((0, 0, 0), |replica| {
                replica.operational_metadata_counts_for_scale()
            });
        let combined_retained_estimate =
            publication_retained_estimate.saturating_add(mutable_replica_retained_estimate);
        let expected_nodes = if layout == "issues-v4-mix" {
            u64::from(total).saturating_mul(235) / 100
        } else {
            u64::from(total)
        };
        let key = NodeKey {
            schema: schema(),
            node: NodeId::new(if layout == "issues-v4-mix" {
                (u64::from(total / 2) << 16).to_be_bytes().to_vec()
            } else {
                (total / 2).to_be_bytes().to_vec()
            })
            .expect("probe"),
        };
        let lookup_started = std::time::Instant::now();
        for _ in 0..100_000 {
            assert!(corpus.node(&key).is_some());
        }
        let lookup_elapsed = lookup_started.elapsed();
        struct ByteTokens;
        impl crate::find_evaluator::TokenCounter for ByteTokens {
            fn count(&self, bytes: &[u8]) -> Result<u64, &'static str> {
                Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            }
        }
        let query_bound: Bound = Policy::default().bound;
        let range_query = Query {
            schema: schema(),
            publication: None,
            mode: Mode::Exact,
            steps: vec![Step {
                id: StepId::new(1).expect("step"),
                input: Vec::new(),
                op: Op::Seek(Seek::Field(Predicate {
                    field: FieldRef {
                        schema: schema(),
                        name: SchemaId::parse(if layout == "issues-v4-mix" {
                            "scalar_0"
                        } else {
                            "title"
                        })
                        .expect("field"),
                    },
                    test: Test::GreaterOrEqual,
                    value: Atom::Text(if layout == "issues-v4-mix" {
                        "search".to_owned()
                    } else {
                        "shared".to_owned()
                    }),
                })),
                bound: query_bound,
            }],
            output: StepId::new(1).expect("output"),
            bound: query_bound,
            page_size: 100,
            cursor: None,
        };
        let range_started = std::time::Instant::now();
        let page = crate::find_evaluator::evaluate(crate::find_evaluator::Evaluation {
            query: &range_query,
            corpus: &corpus,
            gates: &crate::find_evaluator::GrantedGates::new([GateRef {
                schema: schema(),
                name: SchemaId::parse("read").expect("gate"),
            }]),
            admitted_bound: query_bound,
            cursor_position: None,
            feature_scorer: None,
            token_counter: &ByteTokens,
        })
        .expect("100k ordered range page");
        let range_elapsed = range_started.elapsed();
        assert_eq!(
            page.usage.postings_read, 101,
            "page plus one visible look-ahead"
        );
        assert_eq!(page.usage.nodes_visited, 101);
        assert!(page.next_position.is_some());
        let retained_per_node = work.retained_bytes / expected_nodes.max(1);
        assert!(
            retained_per_node <= MAX_LOGICAL_BYTES_PER_NODE,
            "logical retained bytes/node regressed: {retained_per_node} > {MAX_LOGICAL_BYTES_PER_NODE}"
        );
        assert!(
            lookup_elapsed <= MAX_LOOKUP_100K,
            "100k lookup latency regressed: {lookup_elapsed:?} > {MAX_LOOKUP_100K:?}"
        );
        if corpus_rss != 0 {
            let rss_per_node =
                corpus_rss / usize::try_from(expected_nodes.max(1)).expect("node count");
            assert!(
                rss_per_node <= MAX_CORPUS_RSS_BYTES_PER_NODE,
                "{layout} corpus RSS/node regressed: {rss_per_node} > {MAX_CORPUS_RSS_BYTES_PER_NODE}"
            );
            assert!(
                corpus.retained_bytes_estimate() >= usize_u64(corpus_rss),
                "physical retained estimator must dominate observed steady RSS"
            );
        }
        if snapshot_rss != 0 {
            assert!(
                snapshot_retained_estimate >= usize_u64(snapshot_rss),
                "snapshot retained estimator must dominate observed cold-directory RSS"
            );
        }
        if mutable_replica_rss != 0 {
            assert!(
                mutable_replica_retained_estimate >= usize_u64(mutable_replica_rss),
                "mutable Replica retained estimator must dominate record-directory RSS"
            );
        }
        if combined_steady_rss != 0 {
            assert!(
                publication_retained_estimate
                    >= usize_u64(after_build.saturating_sub(after_replica)),
                "combined retained estimator must dominate snapshot plus Corpus RSS"
            );
        }
        assert_eq!(work.retained_bytes, corpus.retained_bytes());
        let expected_postings = if layout == "issues-v4-mix" {
            u64::from(total).saturating_mul(3_440) / 100
        } else {
            u64::from(total) * 5
        };
        assert_eq!(work.nodes_inserted, expected_nodes);
        assert_eq!(work.postings_inserted, expected_postings);
        let mut body_rows_bytes = 0u64;
        let mut node_rows_bytes = 0u64;
        let mut field_rows_bytes = 0u64;
        let mut edge_rows_bytes = 0u64;
        let mut feature_rows_bytes = 0u64;
        let mut value_rows_bytes = 0u64;
        let mut value_slab_bytes = 0u64;
        let mut term_restart_bytes = 0u64;
        let mut term_slab_bytes = 0u64;
        let mut node_key_rows_bytes = 0u64;
        let mut node_key_slab_bytes = 0u64;
        let mut posting_bytes = 0u64;
        let mut segment_arc_headers = 0u64;
        for segment in corpus.packed.segments.iter().flatten() {
            body_rows_bytes = body_rows_bytes
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.body_rows)))
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.body_nodes)));
            node_rows_bytes =
                node_rows_bytes.saturating_add(usize_u64(std::mem::size_of_val(&*segment.nodes)));
            field_rows_bytes =
                field_rows_bytes.saturating_add(usize_u64(std::mem::size_of_val(&*segment.fields)));
            edge_rows_bytes = edge_rows_bytes
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.edges)))
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.targets)));
            feature_rows_bytes = feature_rows_bytes
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.features)))
                .saturating_add(usize_u64(segment.feature_bytes.len()));
            value_rows_bytes = value_rows_bytes
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.value_payloads)))
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.value_meta)));
            value_slab_bytes =
                value_slab_bytes.saturating_add(usize_u64(segment.value_bytes.len()));
            term_restart_bytes = term_restart_bytes
                .saturating_add(usize_u64(segment.terms.blocks.len()).saturating_mul(4));
            term_slab_bytes = term_slab_bytes.saturating_add(usize_u64(segment.terms.bytes.len()));
            node_key_rows_bytes = node_key_rows_bytes
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.node_keys)));
            node_key_slab_bytes =
                node_key_slab_bytes.saturating_add(usize_u64(segment.node_key_bytes.len()));
            posting_bytes = posting_bytes
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.schema_groups)))
                .saturating_add(segment.schema_nodes.retained_bytes())
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.ordered_groups)))
                .saturating_add(segment.ordered_values.retained_bytes())
                .saturating_add(segment.ordered_nodes.retained_bytes())
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.term_groups)))
                .saturating_add(segment.term_ids.retained_bytes())
                .saturating_add(segment.term_nodes.retained_bytes())
                .saturating_add(segment.term_frequencies.retained_bytes())
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.feature_groups)))
                .saturating_add(segment.feature_nodes.retained_bytes())
                .saturating_add(usize_u64(std::mem::size_of_val(&*segment.incoming_groups)))
                .saturating_add(segment.incoming_targets.retained_bytes())
                .saturating_add(segment.incoming_nodes.retained_bytes());
            // Each Arc slice has one allocation header. The exact allocator
            // class is platform-specific; 16 bytes is the minimum accounted
            // header and RSS residual below captures class/PVector slack.
            segment_arc_headers = segment_arc_headers.saturating_add(30 * 16);
        }
        let reference_dictionary_bytes =
            corpus
                .packed
                .segments
                .iter()
                .flatten()
                .fold(0u64, |bytes, segment| {
                    bytes
                        .saturating_add(usize_u64(std::mem::size_of_val(&*segment.schemas)))
                        .saturating_add(usize_u64(std::mem::size_of_val(&*segment.field_names)))
                        .saturating_add(usize_u64(std::mem::size_of_val(&*segment.edge_names)))
                        .saturating_add(usize_u64(std::mem::size_of_val(&*segment.feature_names)))
                        .saturating_add(usize_u64(std::mem::size_of_val(&*segment.gates)))
                        .saturating_add(usize_u64(std::mem::size_of_val(&*segment.visibilities)))
                });
        let point_directory_bytes = corpus.packed.nodes.leaves.iter().fold(0u64, |bytes, leaf| {
            bytes
                .saturating_add(usize_u64(std::mem::size_of_val(&**leaf)))
                .saturating_add(16)
        });
        let body_owner_bytes = usize_u64(corpus.packed.bodies.len())
            .saturating_mul(usize_u64(std::mem::size_of::<Option<PackedBodyRef>>()));
        let value_term_key_bytes = value_rows_bytes
            .saturating_add(value_slab_bytes)
            .saturating_add(term_restart_bytes)
            .saturating_add(term_slab_bytes)
            .saturating_add(node_key_rows_bytes)
            .saturating_add(node_key_slab_bytes);
        let accounted = body_rows_bytes
            .saturating_add(node_rows_bytes)
            .saturating_add(field_rows_bytes)
            .saturating_add(edge_rows_bytes)
            .saturating_add(feature_rows_bytes)
            .saturating_add(value_term_key_bytes)
            .saturating_add(reference_dictionary_bytes)
            .saturating_add(posting_bytes)
            .saturating_add(point_directory_bytes)
            .saturating_add(body_owner_bytes)
            .saturating_add(segment_arc_headers);
        eprintln!(
            "corpus-scale layout={layout} nodes={} bodies={} streaming={} row_ms={} build_ms={} lookup_100k_us={} range_page_us={} range_visited={} process_baseline_rss_mib={:.1} engine_rss_mib={:.1} mutable_replica_rss_mib={:.1} snapshot_steady_rss_mib={:.1} corpus_steady_rss_mib={:.1} combined_steady_rss_mib={:.1} combined_peak_rss_mib={:.1} corpus_rss_bytes_per_node={} logical_mib={:.1} logical_bytes_per_node={} postings={}",
            corpus.node_count(),
            corpus.body_count(),
            streaming,
            rows_elapsed.as_millis(),
            build_elapsed.as_millis(),
            lookup_elapsed.as_micros(),
            range_elapsed.as_micros(),
            page.usage.postings_read,
            before_rows as f64 / (1024.0 * 1024.0),
            after_engine.saturating_sub(before_rows) as f64 / (1024.0 * 1024.0),
            mutable_replica_rss as f64 / (1024.0 * 1024.0),
            snapshot_rss as f64 / (1024.0 * 1024.0),
            corpus_rss as f64 / (1024.0 * 1024.0),
            combined_steady_rss as f64 / (1024.0 * 1024.0),
            combined_peak_rss as f64 / (1024.0 * 1024.0),
            after_build.saturating_sub(before_build)
                / usize::try_from(expected_nodes.max(1)).expect("node count"),
            work.retained_bytes as f64 / (1024.0 * 1024.0),
            retained_per_node,
            work.postings_inserted,
        );
        eprintln!(
            "corpus-shape segments={} ordered={} term_postings={} incoming={} value_dict={} term_dict={} target_dict={} target_bytes={} actual_nodes={} posting_groups={} posting_stream_mib={:.1} receipts={} content_declarations={} unique_content={} mutable_replica_retained_estimate_mib={:.1} snapshot_retained_estimate_mib={:.1} corpus_retained_estimate_mib={:.1} combined_retained_estimate_mib={:.1}",
            corpus.packed.segments.len(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.ordered_nodes.len()).sum::<usize>(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.term_nodes.len()).sum::<usize>(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.incoming_nodes.len()).sum::<usize>(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.value_payloads.len()).sum::<usize>(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.terms.len()).sum::<usize>(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.node_keys.len()).sum::<usize>(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.node_key_bytes.len()).sum::<usize>(),
            corpus.packed.nodes.len(),
            corpus.packed.segments.iter().flatten().map(|segment| segment.schema_groups.len().saturating_add(segment.ordered_groups.len()).saturating_add(segment.term_groups.len()).saturating_add(segment.feature_groups.len()).saturating_add(segment.incoming_groups.len())).sum::<usize>(),
            corpus.packed.segments.iter().flatten().fold(0u64, |bytes, segment| bytes
                .saturating_add(segment.schema_nodes.retained_bytes())
                .saturating_add(segment.ordered_values.retained_bytes())
                .saturating_add(segment.ordered_nodes.retained_bytes())
                .saturating_add(segment.term_ids.retained_bytes())
                .saturating_add(segment.term_nodes.retained_bytes())
                .saturating_add(segment.feature_nodes.retained_bytes())
                .saturating_add(segment.incoming_targets.retained_bytes())
                .saturating_add(segment.incoming_nodes.retained_bytes())) as f64 / (1024.0 * 1024.0),
            receipt_count,
            declared_content_count,
            unique_content_count,
            mutable_replica_retained_estimate as f64 / (1024.0 * 1024.0),
            snapshot_retained_estimate as f64 / (1024.0 * 1024.0),
            corpus_retained_estimate as f64 / (1024.0 * 1024.0),
            combined_retained_estimate as f64 / (1024.0 * 1024.0),
        );
        eprintln!(
            "corpus-layout-mib body={:.2} node={:.2} field={:.2} edge={:.2} feature={:.2} value_term_key={:.2} refs={:.2} postings={:.2} point_dir={:.2} body_owner={:.2} arc_headers={:.2} accounted={:.2} rss_residual={:.2}",
            body_rows_bytes as f64 / (1024.0 * 1024.0),
            node_rows_bytes as f64 / (1024.0 * 1024.0),
            field_rows_bytes as f64 / (1024.0 * 1024.0),
            edge_rows_bytes as f64 / (1024.0 * 1024.0),
            feature_rows_bytes as f64 / (1024.0 * 1024.0),
            value_term_key_bytes as f64 / (1024.0 * 1024.0),
            reference_dictionary_bytes as f64 / (1024.0 * 1024.0),
            posting_bytes as f64 / (1024.0 * 1024.0),
            point_directory_bytes as f64 / (1024.0 * 1024.0),
            body_owner_bytes as f64 / (1024.0 * 1024.0),
            segment_arc_headers as f64 / (1024.0 * 1024.0),
            accounted as f64 / (1024.0 * 1024.0),
            corpus_rss.saturating_sub(usize::try_from(accounted).unwrap_or(usize::MAX)) as f64
                / (1024.0 * 1024.0),
        );
        eprintln!(
            "corpus-dictionary-mib value_rows={:.2} value_bytes={:.2} term_restarts={:.2} term_bytes={:.2} node_key_rows={:.2} node_key_bytes={:.2}",
            value_rows_bytes as f64 / (1024.0 * 1024.0),
            value_slab_bytes as f64 / (1024.0 * 1024.0),
            term_restart_bytes as f64 / (1024.0 * 1024.0),
            term_slab_bytes as f64 / (1024.0 * 1024.0),
            node_key_rows_bytes as f64 / (1024.0 * 1024.0),
            node_key_slab_bytes as f64 / (1024.0 * 1024.0),
        );
    }

    #[test]
    #[ignore = "release-scale memory and lookup fixture"]
    fn release_scale_100k_links() {
        release_scale(
            100_000,
            "dense",
            scale_extractions,
            scale_record_extraction,
            empty_payload,
            false,
        );
    }

    #[test]
    #[ignore = "release-scale memory and lookup fixture"]
    fn release_scale_1m_links() {
        release_scale(
            1_000_000,
            "dense",
            scale_extractions,
            scale_record_extraction,
            empty_payload,
            false,
        );
    }

    #[test]
    #[ignore = "release-scale one-record-per-Body memory and lookup fixture"]
    fn release_scale_100k_record_bodies() {
        release_scale(
            100_000,
            "record-body",
            scale_record_extractions,
            scale_record_extraction,
            empty_payload,
            true,
        );
    }

    #[test]
    #[ignore = "release-scale one-record-per-Body memory and lookup fixture"]
    fn release_scale_1m_record_bodies() {
        release_scale(
            1_000_000,
            "record-body",
            scale_record_extractions,
            scale_record_extraction,
            empty_payload,
            true,
        );
    }

    #[test]
    #[ignore = "release-scale representative Issues-v4 record mix"]
    fn release_scale_100k_issues_v4_mix() {
        release_scale(
            100_000,
            "issues-v4-mix",
            scale_record_extractions,
            issues_v4_record_extraction,
            issues_v4_payload,
            true,
        );
    }

    #[test]
    #[ignore = "release-scale representative Issues-v4 record mix"]
    fn release_scale_1m_issues_v4_mix() {
        release_scale(
            1_000_000,
            "issues-v4-mix",
            scale_record_extractions,
            issues_v4_record_extraction,
            issues_v4_payload,
            true,
        );
    }

    fn combined_record_publication_scale(total: u32) {
        const MAX_COMBINED_RSS: usize = 2 * 1024 * 1024 * 1024;
        let template_key = fabric::Key::from_bytes(b"combined-record-template".to_vec());
        let mut engine = fabric::Engine::new();
        engine
            .commit(fabric::Transaction::new(
                "record-template",
                vec![
                    fabric::Op::CreateBody {
                        key: template_key.clone(),
                    },
                    fabric::Op::RegisterSet {
                        key: template_key.clone(),
                        path: "kind".to_owned(),
                        value: b"relation".to_vec(),
                    },
                ],
            ))
            .expect("template commit");
        let binding = replica::body::BodyBinding {
            schema: SchemaId::parse("issues.link").expect("schema"),
            schema_version: 1,
            encoding: EncodingId::parse("collab").expect("encoding"),
            mutation_model: replica::body::MUTATION_COLLABORATIVE,
        };

        let before = resident_bytes();
        let snapshot_started = std::time::Instant::now();
        let snapshot = Arc::new(replica::ReadSnapshot::from_cold_body_rows_for_test(
            (0..total).map(|number| {
                (
                    scale_body(u64::from(number)),
                    binding.clone(),
                    number.to_be_bytes().to_vec(),
                    cold_scale_material(number, 296),
                )
            }),
        ));
        let snapshot_elapsed = snapshot_started.elapsed();
        let corpus_started = std::time::Instant::now();
        let mut builder = CorpusBuilder::new(coordinate(1, 1), Limits::default(), snapshot.clone());
        for number in 0..total {
            builder
                .push(scale_record_extraction(number, total))
                .expect("streamed record corpus");
        }
        let (corpus, _) = builder.finish().expect("finish combined record corpus");
        let corpus_elapsed = corpus_started.elapsed();
        let after = resident_bytes();
        assert!(engine.body_snapshot(&template_key).unwrap().is_some());
        let rss = after.saturating_sub(before);
        let estimate = snapshot
            .retained_bytes_estimate()
            .saturating_add(corpus.retained_bytes_estimate());
        assert_eq!(snapshot.body_count(), u64::from(total));
        assert_eq!(
            corpus.body_count(),
            usize::try_from(total).expect("Body count")
        );
        assert!(
            estimate <= MAX_COMBINED_RSS as u64,
            "one supported million-record publication must fit one lease"
        );
        if rss != 0 {
            assert!(
                rss <= MAX_COMBINED_RSS,
                "combined publication RSS regressed: {rss} > {MAX_COMBINED_RSS}"
            );
        }
        eprintln!(
            "combined-record-publication bodies={total} snapshot_ms={} corpus_ms={} rss_mib={:.1} snapshot_estimate_mib={:.1} corpus_estimate_mib={:.1} lease_estimate_mib={:.1}",
            snapshot_elapsed.as_millis(),
            corpus_elapsed.as_millis(),
            rss as f64 / (1024.0 * 1024.0),
            snapshot.retained_bytes_estimate() as f64 / (1024.0 * 1024.0),
            corpus.retained_bytes_estimate() as f64 / (1024.0 * 1024.0),
            estimate as f64 / (1024.0 * 1024.0),
        );
    }

    #[test]
    #[ignore = "release-scale combined one-record-per-Body publication residency fixture"]
    fn combined_100k_record_body_publication() {
        combined_record_publication_scale(100_000);
    }

    #[test]
    #[ignore = "release-scale combined one-record-per-Body publication residency fixture"]
    fn combined_1m_record_body_publication() {
        combined_record_publication_scale(1_000_000);
    }

    fn hot_issue_extraction(text: &str, generation: u64) -> BodyExtraction {
        let schema = schema();
        BodyExtraction {
            body: body(77),
            stamp: generation.to_be_bytes().to_vec(),
            nodes: vec![ExtractedNode {
                key: NodeKey {
                    schema: schema.clone(),
                    node: NodeId::new(vec![77]).expect("issue node"),
                },
                gate: Some(GateRef {
                    schema: schema.clone(),
                    name: SchemaId::parse("read").expect("gate"),
                }),
                fields: vec![ExtractedField {
                    reference: FieldRef {
                        schema,
                        name: SchemaId::parse("description").expect("field"),
                    },
                    value: Value::text(text.to_owned()),
                    gate: None,
                    terms: vec![Arc::from(&b"issue"[..])],
                }],
                edges: Vec::new(),
                features: Vec::new(),
            }],
        }
    }

    fn incompressible_ascii(bytes: usize) -> String {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut out = String::with_capacity(bytes);
        for _ in 0..bytes {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push(char::from(
                b'!' + u8::try_from(state % 90).expect("printable"),
            ));
        }
        out
    }

    #[test]
    #[ignore = "release hot-Body Corpus replacement and generation-retention fixture"]
    fn a_one_megabyte_issue_replaces_only_its_body_across_64_corpora() {
        const TEXT_BYTES: usize = 1024 * 1024;
        const GENERATIONS: usize = 64;
        const MAX_PROJECT: std::time::Duration = std::time::Duration::from_millis(100);
        const MAX_APPLY: std::time::Duration = std::time::Duration::from_millis(100);
        const MAX_RETAINED_RSS: usize = 192 * 1024 * 1024;

        let mut text = incompressible_ascii(TEXT_BYTES);
        let (mut current, _) = build_test(
            coordinate(1, 1),
            Limits::default(),
            vec![hot_issue_extraction(&text, 1)],
        )
        .expect("initial issue corpus");
        let mut retained = vec![current.clone()];
        let before = resident_bytes();
        let mut project = Vec::with_capacity(GENERATIONS);
        let mut apply = Vec::with_capacity(GENERATIONS);
        for generation in 0..GENERATIONS {
            text.replace_range(
                generation..generation + 1,
                if generation % 2 == 0 { "x" } else { "y" },
            );
            let project_started = std::time::Instant::now();
            let body = hot_issue_extraction(&text, u64::try_from(generation + 2).expect("stamp"));
            project.push(project_started.elapsed());
            let next_coordinate = coordinate(
                u8::try_from(generation + 2).expect("publication root"),
                u64::try_from(generation + 2).expect("materialization"),
            );
            let apply_started = std::time::Instant::now();
            let (next, work) = current
                .apply(CorpusDelta {
                    base: current.coordinate(),
                    next: next_coordinate,
                    snapshot: current.snapshot.clone(),
                    bodies: vec![body],
                })
                .expect("single-Body replacement");
            apply.push(apply_started.elapsed());
            assert_eq!(work.bodies_replaced, 1);
            assert_eq!(work.nodes_removed, 1);
            assert_eq!(work.nodes_inserted, 1);
            current = next;
            // The current publication must not retain every superseded 1 MiB
            // scalar through its intern dictionary. Older values live only in
            // the explicitly retained immutable generations below.
            assert_eq!(current.values.values.len(), 1);
            retained.push(current.clone());
        }
        let after = resident_bytes();
        let retained_rss = after.saturating_sub(before);
        project.sort();
        apply.sort();
        let project_p99 = project[project.len() * 99 / 100];
        let apply_p99 = apply[apply.len() * 99 / 100];
        assert!(project_p99 <= MAX_PROJECT, "projection p99={project_p99:?}");
        assert!(apply_p99 <= MAX_APPLY, "Corpus apply p99={apply_p99:?}");
        if retained_rss != 0 {
            assert!(retained_rss <= MAX_RETAINED_RSS);
        }
        let current_bytes = current.retained_bytes();
        assert!(current_bytes >= u64::try_from(TEXT_BYTES).unwrap());
        assert!(current_bytes <= u64::try_from(TEXT_BYTES + 4 * 1024).unwrap());
        eprintln!(
            "hot-issue-corpus text_mib=1 generations={} project_p99_us={} apply_p99_us={} retained_rss_mib={:.1} current_value_interns={}",
            retained.len(),
            project_p99.as_micros(),
            apply_p99.as_micros(),
            retained_rss as f64 / (1024.0 * 1024.0),
            current.values.values.len(),
        );
    }
}
