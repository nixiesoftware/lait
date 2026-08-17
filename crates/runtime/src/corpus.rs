#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "Find corpus indexes use bounded binary search and compact u32 ids"
)]
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

use std::{
    borrow::Borrow,
    cmp::Reverse,
    collections::{BTreeSet, BinaryHeap},
    hash::Hash,
    sync::Arc,
};

use imbl::{HashMap as PersistentMap, OrdSet as PersistentSet, Vector as PersistentVector};

use replica::body::BodyKey;

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
    field: Arc<FieldRef>,
    value: Arc<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TermKey {
    field: Arc<FieldRef>,
    term: Arc<[u8]>,
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
struct BodyIx(u32);

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

#[derive(Debug, Clone, Copy)]
struct Span {
    start: u32,
    len: u32,
}

impl Span {
    fn range(self, total: usize) -> Option<std::ops::Range<usize>> {
        let start = usize::try_from(self.start).ok()?;
        let len = usize::try_from(self.len).ok()?;
        let end = start.checked_add(len)?;
        (end <= total).then_some(start..end)
    }
}

#[derive(Debug, Clone)]
struct StoredField {
    reference: Arc<FieldRef>,
    value: Arc<Value>,
    gate: Option<Arc<crate::find::GateRef>>,
    terms: Arc<[Arc<[u8]>]>,
}

#[derive(Debug, Clone)]
struct StoredEdge {
    reference: Arc<EdgeRef>,
    gate: Arc<crate::find::GateRef>,
    targets: Arc<[Arc<NodeKey>]>,
}

#[derive(Debug, Clone)]
struct StoredFeature {
    reference: Arc<FeatureRef>,
    gate: Option<Arc<crate::find::GateRef>>,
    value: Arc<[u8]>,
}

#[derive(Debug, Clone)]
struct NodeColumn {
    key: Arc<NodeKey>,
    gate: Option<Arc<crate::find::GateRef>>,
    fields: Span,
    edges: Span,
    features: Span,
}

/// One Body's immutable offset columns. Replacing another Body shares this
/// allocation whole; visiting a posting materializes only the named row.
#[derive(Debug)]
struct BodyColumns {
    nodes: Arc<[NodeColumn]>,
    fields: Arc<[StoredField]>,
    edges: Arc<[StoredEdge]>,
    features: Arc<[StoredFeature]>,
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
    source: Arc<BodyKey>,
    stamp: Arc<[u8]>,
    nodes: BodyNodes,
    columns: Arc<BodyColumns>,
    retained_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct NodeLoc {
    body: BodyIx,
    row: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OrderedNode {
    key: Arc<NodeKey>,
    node: NodeIx,
}

/// The authority dimensions which must both be admitted before an index row
/// exists for one evaluator. Keeping these as posting partitions—not tags on
/// individual entries—means denied populations are never walked, metered, or
/// chosen as cursor look-ahead.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    first: Option<(Visibility, PersistentSet<T>)>,
    more: Vec<(Visibility, PersistentSet<T>)>,
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
    fn iter(&self) -> impl Iterator<Item = (&Visibility, &PersistentSet<T>)> {
        self.first
            .iter()
            .map(|(visibility, posting)| (visibility, posting))
            .chain(
                self.more
                    .iter()
                    .map(|(visibility, posting)| (visibility, posting)),
            )
    }

    fn posting_mut(&mut self, visibility: Visibility) -> &mut PersistentSet<T> {
        if self.first.is_none() {
            self.first = Some((visibility, PersistentSet::new()));
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
        self.more.push((visibility, PersistentSet::new()));
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
    values: PersistentMap<Arc<T>, u32>,
}

impl<T> Default for Intern<T> {
    fn default() -> Self {
        Self {
            values: PersistentMap::new(),
        }
    }
}

impl<T: Clone + Eq + Hash> Intern<T> {
    fn get(&self, value: &T) -> Option<Arc<T>> {
        self.values
            .get_key_value(value)
            .map(|(held, _)| held.clone())
    }

    fn intern(&mut self, value: T) -> Arc<T> {
        if let Some((held, count)) = self
            .values
            .get_key_value(&value)
            .map(|(held, count)| (held.clone(), *count))
        {
            self.values.insert(held.clone(), count.saturating_add(1));
            return held;
        }
        let value = Arc::new(value);
        self.values.insert(value.clone(), 1);
        value
    }

    fn release(&mut self, value: &T) {
        let Some((held, count)) = self
            .values
            .get_key_value(value)
            .map(|(held, count)| (held.clone(), *count))
        else {
            return;
        };
        if count > 1 {
            self.values.insert(held, count - 1);
        } else {
            self.values.remove(value);
        }
    }
}

#[derive(Debug, Clone, Default)]
struct BytesIntern {
    values: PersistentMap<Arc<[u8]>, u32>,
}

impl BytesIntern {
    fn get(&self, value: &[u8]) -> Option<Arc<[u8]>> {
        self.values
            .get_key_value(value)
            .map(|(held, _)| held.clone())
    }

    fn intern(&mut self, value: Arc<[u8]>) -> Arc<[u8]> {
        if let Some((held, count)) = self
            .values
            .get_key_value(value.as_ref())
            .map(|(held, count)| (held.clone(), *count))
        {
            self.values.insert(held.clone(), count.saturating_add(1));
            return held;
        }
        self.values.insert(value.clone(), 1);
        value
    }

    fn release(&mut self, value: &[u8]) {
        let Some((held, count)) = self
            .values
            .get_key_value(value)
            .map(|(held, count)| (held.clone(), *count))
        else {
            return;
        };
        if count > 1 {
            self.values.insert(held, count - 1);
        } else {
            self.values.remove(value);
        }
    }
}

/// One ready immutable corpus.
#[derive(Debug, Clone)]
pub(crate) struct Corpus {
    coordinate: WorldPublicationId,
    limits: Limits,
    bodies: ChunkedDirectory<Arc<BodyKey>, BodyIx>,
    body_rows: PersistentVector<Option<BodyRows>>,
    free_bodies: PersistentSet<BodyIx>,
    nodes: ChunkedDirectory<Arc<NodeKey>, NodeIx>,
    node_rows: PersistentVector<Option<NodeLoc>>,
    free_nodes: PersistentSet<NodeIx>,
    schemas: PersistentMap<Arc<SchemaRef>, PartitionedOrderedNodes>,
    ordered_fields: PersistentMap<Arc<FieldRef>, PartitionedOrderedField>,
    exact: PersistentMap<ExactKey, PartitionedPosting>,
    /// Exact-token and bounded-prefix postings, each canonically ordered by
    /// NodeKey. Prefix postings are materialized once per publication so a
    /// page never unions or rescans a term dictionary at read time.
    terms: PersistentMap<TermKey, PartitionedOrderedNodes>,
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
    feature_names: Intern<FeatureRef>,
    node_names: Intern<NodeKey>,
    values: Intern<Value>,
    term_bytes: BytesIntern,
    retained_bytes: u64,
}

impl Corpus {
    /// Build a complete corpus from explicit extractor rows.
    pub fn build(
        coordinate: WorldPublicationId,
        limits: Limits,
        bodies: Vec<BodyExtraction>,
    ) -> Result<(Self, BuildWork), Failure> {
        let empty = Self {
            coordinate,
            limits,
            bodies: ChunkedDirectory::default(),
            body_rows: PersistentVector::new(),
            free_bodies: PersistentSet::new(),
            nodes: ChunkedDirectory::default(),
            node_rows: PersistentVector::new(),
            free_nodes: PersistentSet::new(),
            schemas: PersistentMap::new(),
            ordered_fields: PersistentMap::new(),
            exact: PersistentMap::new(),
            terms: PersistentMap::new(),
            features: PersistentMap::new(),
            incoming: Partitioned::default(),
            schema_names: Intern::default(),
            field_names: Intern::default(),
            edge_names: Intern::default(),
            gate_names: Intern::default(),
            feature_names: Intern::default(),
            node_names: Intern::default(),
            values: Intern::default(),
            term_bytes: BytesIntern::default(),
            retained_bytes: 0,
        };
        let delta = CorpusDelta {
            base: coordinate,
            next: coordinate,
            bodies,
        };
        empty.apply_inner(delta, true)
    }

    /// Apply complete replacements for only the Bodies named by `delta`.
    ///
    /// The original corpus is unchanged on every error. Empty replacement
    /// batches are permitted because a Space-wide Manifest root or World
    /// implementation can move while this World's extracted rows remain
    /// byte-equivalent and therefore fully shared.
    pub fn apply(&self, delta: CorpusDelta) -> Result<(Self, BuildWork), Failure> {
        self.apply_inner(delta, false)
    }

    fn apply_inner(
        &self,
        delta: CorpusDelta,
        building: bool,
    ) -> Result<(Self, BuildWork), Failure> {
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
        for body in bodies {
            next.insert_body(body, &mut work)?;
        }
        if next.retained_bytes > next.limits.retained_bytes {
            return Err(Failure::Limit("corpus retained bytes"));
        }
        if building && next.bodies.len() != changed.len() {
            return Err(Failure::Invalid("full build body accounting"));
        }
        work.retained_bytes = next.retained_bytes;
        Ok((next, work))
    }

    fn remove_body(&mut self, body: &BodyKey, rows: &BodyRows, work: &mut BuildWork) {
        let Some(body_ix) = self.bodies.remove(body) else {
            return;
        };
        self.retained_bytes = self.retained_bytes.saturating_sub(rows.retained_bytes);
        for (row_number, node_ix) in rows.nodes.iter().copied().enumerate() {
            let Some(column) = rows.columns.nodes.get(row_number) else {
                continue;
            };
            let key = &column.key;
            let node_visibility = Visibility::node(column.gate.clone());
            self.nodes.remove(key.as_ref());
            self.clear_node_slot(node_ix);
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
            for field in span_slice(&rows.columns.fields, column.fields) {
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
                let exact = ExactKey {
                    field: field.reference.clone(),
                    value: field.value.clone(),
                };
                partitioned_remove(&mut self.exact, &exact, &visibility, &node_ix, work);
                for term in field.terms.iter() {
                    partitioned_remove(
                        &mut self.terms,
                        &TermKey {
                            field: field.reference.clone(),
                            term: term.clone(),
                        },
                        &visibility,
                        &OrderedNode {
                            key: key.clone(),
                            node: node_ix,
                        },
                        work,
                    );
                    self.term_bytes.release(term.as_ref());
                }
                self.field_names.release(field.reference.as_ref());
                self.values.release(field.value.as_ref());
                if let Some(gate) = &field.gate {
                    self.gate_names.release(gate.as_ref());
                }
            }
            for feature in span_slice(&rows.columns.features, column.features) {
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
            for edge in span_slice(&rows.columns.edges, column.edges) {
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
        }
        self.clear_body_slot(body_ix);
    }

    fn insert_body(
        &mut self,
        mut body: BodyExtraction,
        work: &mut BuildWork,
    ) -> Result<(), Failure> {
        canonicalize_extraction(&mut body);
        if self.bodies.contains_key(&body.body) {
            return Err(Failure::DuplicateBody(body.body));
        }
        let body_ix = self.allocate_body_slot()?;
        let source = Arc::new(body.body.clone());
        let mut node_ixs = Vec::with_capacity(body.nodes.len());
        let mut nodes = Vec::with_capacity(body.nodes.len());
        let mut fields = Vec::new();
        let mut edges = Vec::new();
        let mut features = Vec::new();
        let mut body_bytes = usize_u64(body.stamp.len());

        for row in body.nodes {
            if self.nodes.get(&row.key).is_some() {
                return Err(Failure::DuplicateNode(row.key));
            }
            body_bytes = body_bytes.saturating_add(retained_node_bytes(&row));
            let row_number =
                u32::try_from(nodes.len()).map_err(|_| Failure::Limit("corpus node rows"))?;
            let node_ix = self.allocate_node_slot(NodeLoc {
                body: body_ix,
                row: row_number,
            })?;
            let key = self.node_names.intern(row.key);
            let schema = self.schema_names.intern(key.schema.clone());
            let node_gate = row.gate.map(|gate| self.gate_names.intern(gate));
            let node_visibility = Visibility::node(node_gate.clone());
            let fields_start = fields.len();
            let edges_start = edges.len();
            let features_start = features.len();

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
                partitioned_insert(
                    &mut self.exact,
                    ExactKey {
                        field: reference.clone(),
                        value: value.clone(),
                    },
                    visibility.clone(),
                    node_ix,
                    work,
                );
                for term in &terms {
                    partitioned_insert(
                        &mut self.terms,
                        TermKey {
                            field: reference.clone(),
                            term: term.clone(),
                        },
                        visibility.clone(),
                        OrderedNode {
                            key: key.clone(),
                            node: node_ix,
                        },
                        work,
                    );
                }
                fields.push(StoredField {
                    reference,
                    value,
                    gate: field_gate,
                    terms: Arc::from(terms),
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
                    targets: Arc::from(targets),
                });
            }
            nodes.push(NodeColumn {
                key: key.clone(),
                gate: node_gate,
                fields: make_span(fields_start, fields.len())?,
                edges: make_span(edges_start, edges.len())?,
                features: make_span(features_start, features.len())?,
            });
            if self.nodes.insert(key, node_ix).is_some() {
                return Err(Failure::Invalid("node index replacement"));
            }
            node_ixs.push(node_ix);
            work.nodes_inserted = work.nodes_inserted.saturating_add(1);
        }

        let rows = BodyRows {
            source: source.clone(),
            stamp: Arc::from(body.stamp),
            nodes: BodyNodes::from_vec(node_ixs),
            columns: Arc::new(BodyColumns {
                nodes: Arc::from(nodes),
                fields: Arc::from(fields),
                edges: Arc::from(edges),
                features: Arc::from(features),
            }),
            retained_bytes: body_bytes,
        };
        self.set_body_slot(body_ix, rows)?;
        if self.bodies.insert(source, body_ix).is_some() {
            return Err(Failure::Invalid("body index replacement"));
        }
        self.retained_bytes = self.retained_bytes.saturating_add(body_bytes);
        Ok(())
    }

    fn body_rows_for(&self, body: &BodyKey) -> Option<BodyRows> {
        let body_ix = *self.bodies.get(body)?;
        self.body_rows.get(body_ix.0 as usize)?.as_ref().cloned()
    }

    fn allocate_body_slot(&mut self) -> Result<BodyIx, Failure> {
        if let Some(index) = self.free_bodies.remove_min() {
            return Ok(index);
        }
        let raw = u32::try_from(self.body_rows.len())
            .map_err(|_| Failure::Limit("corpus Body identities"))?;
        self.body_rows.push_back(None);
        Ok(BodyIx(raw))
    }

    fn set_body_slot(&mut self, index: BodyIx, rows: BodyRows) -> Result<(), Failure> {
        let slot = self
            .body_rows
            .get_mut(index.0 as usize)
            .ok_or(Failure::Invalid("Body slot"))?;
        if slot.replace(rows).is_some() {
            return Err(Failure::Invalid("occupied Body slot"));
        }
        Ok(())
    }

    fn clear_body_slot(&mut self, index: BodyIx) {
        if let Some(slot) = self.body_rows.get_mut(index.0 as usize) {
            *slot = None;
            self.free_bodies.insert(index);
        }
    }

    fn allocate_node_slot(&mut self, location: NodeLoc) -> Result<NodeIx, Failure> {
        if let Some(index) = self.free_nodes.remove_min() {
            let Some(slot) = self.node_rows.get_mut(index.0 as usize) else {
                return Err(Failure::Invalid("Node slot"));
            };
            if slot.replace(location).is_some() {
                return Err(Failure::Invalid("occupied Node slot"));
            }
            return Ok(index);
        }
        let raw = u32::try_from(self.node_rows.len())
            .map_err(|_| Failure::Limit("corpus Node identities"))?;
        self.node_rows.push_back(Some(location));
        Ok(NodeIx(raw))
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

    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub const fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }

    /// O(1) conservative physical-retention price for admission and cache
    /// policy. The fixed terms are calibrated above the release-scale dense
    /// and one-record-per-Body fixtures after the compact directory cutover;
    /// logical extractor bytes remain exact.
    pub fn retained_bytes_estimate(&self) -> u64 {
        self.retained_bytes
            .saturating_add(
                u64::try_from(self.node_count())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(288),
            )
            .saturating_add(
                u64::try_from(self.body_count())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(64),
            )
    }

    pub fn body_stamp(&self, body: &BodyKey) -> Option<Arc<[u8]>> {
        self.body_rows_for(body).map(|rows| rows.stamp.clone())
    }

    pub fn node(&self, key: &NodeKey) -> Option<ExtractedNode> {
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
        let posting = self
            .field_names
            .get(field)
            .zip(self.values.get(value))
            .and_then(|(field, value)| self.exact.get(&ExactKey { field, value }));
        count_partitioned(posting, &allows)
    }

    pub fn count_term(
        &self,
        field: &FieldRef,
        term: &[u8],
        allows: impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> usize {
        let posting = self
            .field_names
            .get(field)
            .zip(self.term_bytes.get(term))
            .and_then(|(field, term)| self.terms.get(&TermKey { field, term }));
        count_partitioned(posting, &allows)
    }

    /// Durable source of one extracted node.
    pub fn source(&self, key: &NodeKey) -> Option<BodyKey> {
        let index = *self.nodes.get(key)?;
        self.row(index)
            .map(|(rows, _)| rows.source.as_ref().clone())
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
        let posting = self
            .field_names
            .get(field)
            .zip(self.values.get(value))
            .and_then(|(field, value)| self.exact.get(&ExactKey { field, value }));
        self.visit_posting(posting, limit, &allows, &mut visit)
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
        mut visit: impl FnMut(&BodyKey, &ExtractedNode) -> bool,
    ) -> Visit {
        if prefix {
            return Visit {
                available: 0,
                visited: 0,
            };
        }
        let posting = self
            .field_names
            .get(field)
            .zip(self.term_bytes.get(term))
            .and_then(|(field, term)| self.terms.get(&TermKey { field, term }));
        self.visit_ordered_nodes(posting, limit, &allows, &mut visit)
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
                let Some((rows, _)) = self.row(entry.source) else {
                    return true;
                };
                let Some(node) = self.materialize_admitted(entry.source, &allows) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(rows.source.as_ref(), &node)
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
                let Some((rows, _)) = self.row(entry.node) else {
                    return true;
                };
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(entry.key.as_ref().clone(), rows.source.as_ref(), &node)
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
        mut visit: impl FnMut(NodeKey, &BodyKey, &ExtractedNode) -> bool,
    ) -> usize {
        if prefix {
            return 0;
        }
        let Some(posting) = self
            .field_names
            .get(field)
            .zip(self.term_bytes.get(term))
            .and_then(|(field, term)| self.terms.get(&TermKey { field, term }))
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
                let Some((rows, _)) = self.row(entry.node) else {
                    return true;
                };
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(entry.key.as_ref().clone(), rows.source.as_ref(), &node)
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
                let Some((rows, _)) = self.row(entry.node) else {
                    return true;
                };
                let Some(node) = self.materialize_admitted(entry.node, &allows) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(
                    entry.value.as_ref().clone(),
                    entry.key.as_ref().clone(),
                    rows.source.as_ref(),
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
            let Some(column) = rows.columns.nodes.get(row) else {
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
                let Some((rows, _)) = self.row(index) else {
                    return true;
                };
                let Some(node) = self.materialize_admitted(index, allows) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(rows.source.as_ref(), &node)
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
                let Some((rows, _)) = self.row(entry.node) else {
                    return true;
                };
                let Some(node) = self.materialize_admitted(entry.node, allows) else {
                    return true;
                };
                visited = visited.saturating_add(1);
                visit(rows.source.as_ref(), &node)
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
            let Some((rows, _)) = self.row(entry.node) else {
                return true;
            };
            let Some(node) = self.materialize_admitted(entry.node, allows) else {
                return true;
            };
            visited = visited.saturating_add(1);
            visit(rows.source.as_ref(), &node)
        });
        Visit {
            available: if bounded { visited } else { available },
            visited,
        }
    }

    fn row(&self, index: NodeIx) -> Option<(&BodyRows, &NodeColumn)> {
        let location = self.node_rows.get(index.0 as usize)?.as_ref()?;
        let rows = self.body_rows.get(location.body.0 as usize)?.as_ref()?;
        let column = rows.columns.nodes.get(location.row as usize)?;
        Some((rows, column))
    }

    fn materialize(&self, index: NodeIx) -> Option<ExtractedNode> {
        self.materialize_admitted(index, &|_| true)
    }

    fn materialize_admitted(
        &self,
        index: NodeIx,
        allows: &impl Fn(Option<&crate::find::GateRef>) -> bool,
    ) -> Option<ExtractedNode> {
        let (rows, column) = self.row(index)?;
        if !allows(column.gate.as_deref()) {
            return None;
        }
        let fields = span_slice(&rows.columns.fields, column.fields)
            .iter()
            .filter(|field| allows(field.gate.as_deref()))
            .map(|field| ExtractedField {
                reference: field.reference.as_ref().clone(),
                value: field.value.as_ref().clone(),
                gate: field.gate.as_ref().map(|gate| gate.as_ref().clone()),
                terms: field.terms.to_vec(),
            })
            .collect();
        let edges = span_slice(&rows.columns.edges, column.edges)
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
        let features = span_slice(&rows.columns.features, column.features)
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

fn make_span(start: usize, end: usize) -> Result<Span, Failure> {
    let len = end
        .checked_sub(start)
        .ok_or(Failure::Invalid("column span"))?;
    Ok(Span {
        start: u32::try_from(start).map_err(|_| Failure::Limit("column offsets"))?,
        len: u32::try_from(len).map_err(|_| Failure::Limit("column offsets"))?,
    })
}

fn span_slice<T>(values: &[T], span: Span) -> &[T] {
    span.range(values.len())
        .and_then(|range| values.get(range))
        .unwrap_or_default()
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
            Atom, Bound, EdgeRef, ExtractedEdge, ExtractedField, FieldRef, GateRef, Mode, NodeId,
            Op, Policy, Predicate, Query, SchemaRef, Seek, Step, StepId, Test,
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

    #[test]
    fn full_build_indexes_schema_exact_value_and_term() {
        let (corpus, work) = Corpus::build(
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
            |source, _| {
                assert_eq!(source, &body(2));
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
        let prefix = corpus.visit_term(&title, b"al", true, 4, |_| true, |_, _| true);
        assert_eq!(
            prefix,
            Visit {
                available: 0,
                visited: 0
            },
            "analyzed Prefix is refused at validation and never materialized"
        );
        assert_eq!(
            corpus.visit_term(&title, b"al", false, 4, |_| true, |_, _| true),
            Visit {
                available: 0,
                visited: 0
            },
            "Token al must not alias Prefix al"
        );
    }

    #[test]
    fn delta_replaces_only_named_body_and_removes_old_postings() {
        let (corpus, _) = Corpus::build(
            coordinate(1, 1),
            Limits::default(),
            vec![extraction(1, "alpha"), extraction(2, "beta")],
        )
        .expect("build");
        let unchanged = corpus.body_rows_for(&body(2)).expect("Body rows");

        let (next, work) = corpus
            .apply(CorpusDelta {
                base: coordinate(1, 1),
                next: coordinate(2, 2),
                bodies: vec![extraction(1, "gamma")],
            })
            .expect("delta");
        assert_eq!(next.node_count(), 2);
        assert_eq!(work.nodes_removed, 1);
        assert_eq!(work.nodes_inserted, 1);
        let still_shared = next.body_rows_for(&body(2)).expect("Body rows");
        assert!(Arc::ptr_eq(&unchanged.columns, &still_shared.columns));

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
    fn coordinate_mismatch_and_duplicate_nodes_fail_without_mutating_base() {
        let (corpus, _) = Corpus::build(
            coordinate(1, 1),
            Limits::default(),
            vec![extraction(1, "alpha")],
        )
        .expect("build");
        let mismatch = corpus.apply(CorpusDelta {
            base: coordinate(9, 9),
            next: coordinate(2, 2),
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
                bodies: vec![duplicate],
            }),
            Err(Failure::DuplicateNode(_))
        ));
        assert_eq!(corpus.coordinate(), coordinate(1, 1));
        assert_eq!(corpus.node_count(), 1);
    }

    #[test]
    fn empty_delta_moves_coordinate_and_shares_body_columns() {
        let (corpus, _) = Corpus::build(
            coordinate(1, 1),
            Limits::default(),
            vec![extraction(1, "alpha")],
        )
        .expect("build");
        let (next, work) = corpus
            .apply(CorpusDelta {
                base: coordinate(1, 1),
                next: coordinate(2, 2),
                bodies: Vec::new(),
            })
            .expect("coordinate-only delta");
        let prior_rows = corpus.body_rows_for(&body(1)).expect("Body rows");
        let next_rows = next.body_rows_for(&body(1)).expect("Body rows");
        assert!(Arc::ptr_eq(&prior_rows.columns, &next_rows.columns));
        assert_eq!(work.nodes_inserted, 0);
        assert_eq!(next.coordinate(), coordinate(2, 2));
    }

    #[test]
    fn bounds_are_checked_before_any_replacement() {
        let limits = Limits {
            value_bytes: 3,
            ..Limits::default()
        };
        let result = Corpus::build(coordinate(1, 1), limits, vec![extraction(1, "large")]);
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
        let (corpus, _) = Corpus::build(
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
        let source_ix = *corpus.nodes.get(&source).expect("source node");
        let (_, source_column) = corpus.row(source_ix).expect("source row");
        let source_edges = span_slice(
            &corpus.row(source_ix).expect("source row").0.columns.edges,
            source_column.edges,
        );
        let stored_target = source_edges[0].targets[0].clone();
        let (interned_target, _) = corpus
            .nodes
            .get_key_value(&target)
            .expect("target identity in node directory");
        assert!(Arc::ptr_eq(&stored_target, interned_target));
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
            .map(|number| BodyExtraction {
                body: scale_body(u64::from(number)),
                stamp: number.to_be_bytes().to_vec(),
                nodes: vec![scale_node(number, total)],
            })
            .collect()
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

    fn release_scale(total: u32, layout: &str, extractions: fn(u32) -> Vec<BodyExtraction>) {
        const MAX_LOGICAL_BYTES_PER_NODE: u64 = 64;
        const MAX_CORPUS_RSS_BYTES_PER_NODE: usize = 4 * 1024;
        const MAX_LOOKUP_100K: std::time::Duration = std::time::Duration::from_secs(5);
        let before_rows = resident_bytes();
        let rows_started = std::time::Instant::now();
        let bodies = extractions(total);
        let rows_elapsed = rows_started.elapsed();
        let before_build = resident_bytes();
        let build_started = std::time::Instant::now();
        let (corpus, work) = Corpus::build(coordinate(1, 1), Limits::default(), bodies)
            .expect("release corpus build");
        let build_elapsed = build_started.elapsed();
        let after_build = resident_bytes();
        let key = NodeKey {
            schema: schema(),
            node: NodeId::new((total / 2).to_be_bytes().to_vec()).expect("probe"),
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
                        name: SchemaId::parse("title").expect("field"),
                    },
                    test: Test::GreaterOrEqual,
                    value: Atom::Text("shared".to_owned()),
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
        let retained_per_node = work.retained_bytes / u64::from(total);
        assert!(
            retained_per_node <= MAX_LOGICAL_BYTES_PER_NODE,
            "logical retained bytes/node regressed: {retained_per_node} > {MAX_LOGICAL_BYTES_PER_NODE}"
        );
        assert!(
            lookup_elapsed <= MAX_LOOKUP_100K,
            "100k lookup latency regressed: {lookup_elapsed:?} > {MAX_LOOKUP_100K:?}"
        );
        let corpus_rss = after_build.saturating_sub(before_build);
        if corpus_rss != 0 {
            let rss_per_node = corpus_rss / usize::try_from(total).expect("node count");
            assert!(
                rss_per_node <= MAX_CORPUS_RSS_BYTES_PER_NODE,
                "{layout} corpus RSS/node regressed: {rss_per_node} > {MAX_CORPUS_RSS_BYTES_PER_NODE}"
            );
        }
        assert_eq!(work.retained_bytes, corpus.retained_bytes());
        assert_eq!(work.nodes_inserted, u64::from(total));
        assert_eq!(
            work.postings_inserted,
            u64::from(total) * 5,
            "schema, ordered/exact Field, exact Token, and incoming Edge"
        );
        eprintln!(
            "corpus-scale layout={layout} nodes={total} bodies={} row_ms={} build_ms={} lookup_100k_us={} range_page_us={} range_visited={} rows_rss_mib={:.1} corpus_rss_mib={:.1} corpus_rss_bytes_per_node={} logical_mib={:.1} logical_bytes_per_node={} postings={}",
            corpus.body_count(),
            rows_elapsed.as_millis(),
            build_elapsed.as_millis(),
            lookup_elapsed.as_micros(),
            range_elapsed.as_micros(),
            page.usage.postings_read,
            before_build.saturating_sub(before_rows) as f64 / (1024.0 * 1024.0),
            after_build.saturating_sub(before_build) as f64 / (1024.0 * 1024.0),
            after_build.saturating_sub(before_build)
                / usize::try_from(total).expect("node count"),
            work.retained_bytes as f64 / (1024.0 * 1024.0),
            retained_per_node,
            work.postings_inserted,
        );
    }

    #[test]
    #[ignore = "release-scale memory and lookup fixture"]
    fn release_scale_100k_links() {
        release_scale(100_000, "dense", scale_extractions);
    }

    #[test]
    #[ignore = "release-scale memory and lookup fixture"]
    fn release_scale_1m_links() {
        release_scale(1_000_000, "dense", scale_extractions);
    }

    #[test]
    #[ignore = "release-scale one-record-per-Body memory and lookup fixture"]
    fn release_scale_100k_record_bodies() {
        release_scale(100_000, "record-body", scale_record_extractions);
    }

    #[test]
    #[ignore = "release-scale one-record-per-Body memory and lookup fixture"]
    fn release_scale_1m_record_bodies() {
        release_scale(1_000_000, "record-body", scale_record_extractions);
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
        let template = engine.export_body(&template_key).expect("template export");
        drop(engine);
        let binding = replica::body::BodyBinding {
            schema: SchemaId::parse("issues.link").expect("schema"),
            schema_version: 1,
            encoding: EncodingId::parse("collab").expect("encoding"),
            mutation_model: replica::body::MUTATION_COLLABORATIVE,
        };

        let before = resident_bytes();
        let snapshot_started = std::time::Instant::now();
        let snapshot = replica::ReadSnapshot::from_body_rows_for_test((0..total).map(|number| {
            let body = fabric::BodySnapshot::from_export(&template_key, template.clone())
                .expect("record Body image");
            (
                scale_body(u64::from(number)),
                binding.clone(),
                number.to_be_bytes().to_vec(),
                body,
            )
        }));
        let snapshot_elapsed = snapshot_started.elapsed();
        let corpus_started = std::time::Instant::now();
        let (corpus, _) = Corpus::build(
            coordinate(1, 1),
            Limits::default(),
            scale_record_extractions(total),
        )
        .expect("record corpus");
        let corpus_elapsed = corpus_started.elapsed();
        let after = resident_bytes();
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
        let (mut current, _) = Corpus::build(
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
