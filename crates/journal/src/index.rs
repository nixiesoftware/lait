//! The canonical persistent authenticated radix index.
//!
//! One mechanism, four uses: the journal's required-object root, the Replica's
//! Body catalog, its content catalog, and the Body history index F3 adds. It is
//! semantics-free like the rest of this crate — keys are 32 bytes, values are
//! opaque bytes, and nothing here knows what either means.
//!
//! **Canonical.** The same logical entry set has exactly one encoding,
//! independent of the order the entries arrived or the edits that produced it.
//! That is what makes a root hash a commitment to a *set* rather than to a
//! history, and it is why the shape rules below are stated as properties of the
//! set rather than as operations: a leaf holds a set of entries iff that set
//! fits a leaf, and splits into a branch iff it does not. Rebuilding from
//! scratch and updating in place produce identical bytes, which
//! `index_fixtures.rs` asserts rather than assumes.
//!
//! **O(changed).** An update descends only the paths its changed keys touch.
//! An untouched subtree is carried forward by reference without being read, so
//! a one-key change rewrites one leaf plus its ancestors — bounded by depth,
//! not by the size of the set. This is the whole point of the module: the
//! measured 28.8 MB rewrite for a one-Body edit at 100k Bodies came from
//! re-encoding complete vectors, and an index is what replaces them.
//!
//! **Bounded.** Keys are hashes, so the tree's depth is bounded by the key
//! width (64 nibbles) and its shape does not degrade on adversarial logical
//! keys. Every node has a size bound, every path a depth bound, and validation
//! checks both before it allocates.

use serde::{Deserialize, Serialize};

/// The index key: a domain-separated hash of the caller's logical key. Callers
/// hash their own logical keys; this crate never sees them.
pub type IndexKey = [u8; 32];

/// Nibbles in a key, and therefore the maximum depth of any path.
pub const MAX_DEPTH: usize = 64;
/// Maximum entries in one leaf before it must split.
pub const MAX_LEAF_ENTRIES: usize = 256;
/// Maximum encoded size of any node.
pub const MAX_NODE_BYTES: usize = 1024 * 1024;
/// Maximum encoded size of one entry's value.
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
/// Branching factor: one nibble per level.
pub const FANOUT: usize = 16;

/// One entry: a key and its opaque value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub key: IndexKey,
    pub value: Vec<u8>,
}

/// A reference to a child subtree: its node hash and how many entries it holds.
/// The count is what makes the merge rule cheap — a branch knows whether its
/// whole subtree would fit one leaf without reading the subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRef {
    pub hash: [u8; 32],
    pub count: u64,
}

/// A node. Leaves hold sorted unique entries; branches hold up to 16 children,
/// one per nibble value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexNode {
    Leaf(Vec<IndexEntry>),
    Branch(Box<[Option<ChildRef>; FANOUT]>),
}

/// Why an index operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// A node did not decode, or re-encoding it did not reproduce its bytes.
    NonCanonical,
    /// A node, value, or path exceeded a protocol bound.
    Bounds,
    /// Entries out of order, duplicated, or misplaced for their prefix.
    Order,
    /// A declared child count disagreed with the subtree it names.
    CountMismatch,
    /// A node named by a parent could not be read.
    MissingNode([u8; 32]),
    /// A leaf held a set that should have split, or a branch a set that should
    /// have merged: the encoding is legal but not the canonical one.
    NotCanonicalShape,
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for IndexError {}

/// Where nodes are read from. The store implements it over its object
/// directory; tests implement it over a map.
pub trait NodeSource {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>>;
}

/// The nibble of `key` at `depth`.
fn nibble(key: &IndexKey, depth: usize) -> usize {
    let byte = key[depth / 2];
    if depth % 2 == 0 {
        (byte >> 4) as usize
    } else {
        (byte & 0x0F) as usize
    }
}

/// The content address of an encoded node — the store's object address, not a
/// separate one.
///
/// Nodes *are* objects. Giving them their own hash domain would mean a
/// `ChildRef` named an address the store had never heard of, and the store
/// would then keep the node alive under one name while the index looked for it
/// under another. They must be the same name or reachability is a fiction.
pub fn node_hash(bytes: &[u8]) -> [u8; 32] {
    crate::object_content_hash(bytes)
}

impl IndexNode {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard index node")
    }

    /// Decode, insisting the encoding was canonical. Bounds are checked before
    /// the decode allocates.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, IndexError> {
        if bytes.len() > MAX_NODE_BYTES {
            return Err(IndexError::Bounds);
        }
        let node: Self = postcard::from_bytes(bytes).map_err(|_| IndexError::NonCanonical)?;
        if node.encode() != bytes {
            return Err(IndexError::NonCanonical);
        }
        Ok(node)
    }
}

/// Whether a set of entries is small enough to live in one leaf at `depth`.
///
/// A single entry always fits: at maximum depth the whole key is consumed and
/// two distinct keys cannot share every nibble, so a deepest leaf holds exactly
/// one entry and must be allowed to whatever size that entry is.
fn fits_leaf(entries: &[IndexEntry], depth: usize) -> bool {
    if entries.len() <= 1 || depth >= MAX_DEPTH {
        return true;
    }
    entries.len() <= MAX_LEAF_ENTRIES
        && IndexNode::Leaf(entries.to_vec()).encode().len() <= MAX_NODE_BYTES
}

/// Collects the nodes an update produced, so the caller can persist exactly the
/// ones it must. An untouched subtree never appears here — that is the
/// O(changed) property, made visible.
#[derive(Debug, Default)]
pub struct NodeSink {
    pub written: Vec<Vec<u8>>,
}

impl NodeSink {
    fn emit(&mut self, node: &IndexNode, count: u64) -> ChildRef {
        let bytes = node.encode();
        let hash = node_hash(&bytes);
        self.written.push(bytes);
        ChildRef { hash, count }
    }
}

/// Build the canonical subtree for an already-sorted, unique entry set at
/// `depth`. A pure function of the set: this is the definition the incremental
/// path must agree with.
fn build(entries: &[IndexEntry], depth: usize, sink: &mut NodeSink) -> Option<ChildRef> {
    if entries.is_empty() {
        return None;
    }
    if fits_leaf(entries, depth) {
        let node = IndexNode::Leaf(entries.to_vec());
        return Some(sink.emit(&node, entries.len() as u64));
    }
    let mut children: [Option<ChildRef>; FANOUT] = Default::default();
    let mut total = 0u64;
    let mut start = 0usize;
    while start < entries.len() {
        let slot = nibble(&entries[start].key, depth);
        let mut end = start;
        while end < entries.len() && nibble(&entries[end].key, depth) == slot {
            end += 1;
        }
        let child = build(&entries[start..end], depth + 1, sink);
        if let Some(c) = child {
            total += c.count;
        }
        children[slot] = child;
        start = end;
    }
    let node = IndexNode::Branch(Box::new(children));
    Some(sink.emit(&node, total))
}

/// Build a canonical index from scratch. Entries need not be sorted; duplicate
/// keys are rejected rather than silently resolved.
pub fn build_index(
    mut entries: Vec<IndexEntry>,
    sink: &mut NodeSink,
) -> Result<Option<ChildRef>, IndexError> {
    for entry in &entries {
        if entry.value.len() > MAX_VALUE_BYTES {
            return Err(IndexError::Bounds);
        }
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    if entries.windows(2).any(|w| w[0].key == w[1].key) {
        return Err(IndexError::Order);
    }
    Ok(build(&entries, 0, sink))
}

/// Read every entry of a subtree, in key order. Bounded by the caller: only
/// called on subtrees whose declared count is already known to be small.
fn collect(
    source: &dyn NodeSource,
    child: &ChildRef,
    depth: usize,
    out: &mut Vec<IndexEntry>,
) -> Result<(), IndexError> {
    if depth > MAX_DEPTH {
        return Err(IndexError::Bounds);
    }
    let bytes = source
        .node(&child.hash)
        .ok_or(IndexError::MissingNode(child.hash))?;
    match IndexNode::decode_canonical(&bytes)? {
        IndexNode::Leaf(entries) => out.extend(entries),
        IndexNode::Branch(children) => {
            for slot in children.iter().flatten() {
                collect(source, slot, depth + 1, out)?;
            }
        }
    }
    Ok(())
}

/// One requested change: set a key to a value, or remove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexChange {
    pub key: IndexKey,
    pub value: Option<Vec<u8>>,
}

/// Apply a batch of changes, rewriting only the paths they touch.
///
/// Returns the new root and, through `sink`, exactly the nodes that had to be
/// written. Nodes the update superseded are not deleted here — they become
/// unreachable, and the store's sweep collects them.
pub fn apply(
    source: &dyn NodeSource,
    root: Option<ChildRef>,
    mut changes: Vec<IndexChange>,
    sink: &mut NodeSink,
) -> Result<Option<ChildRef>, IndexError> {
    for change in &changes {
        if change
            .value
            .as_ref()
            .is_some_and(|v| v.len() > MAX_VALUE_BYTES)
        {
            return Err(IndexError::Bounds);
        }
    }
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    changes.dedup_by(|a, b| a.key == b.key);
    if changes.is_empty() {
        return Ok(root);
    }
    descend(source, root, 0, &changes, sink)
}

fn descend(
    source: &dyn NodeSource,
    node: Option<ChildRef>,
    depth: usize,
    changes: &[IndexChange],
    sink: &mut NodeSink,
) -> Result<Option<ChildRef>, IndexError> {
    // Nothing to do here: carry the subtree forward by reference, unread.
    if changes.is_empty() {
        return Ok(node);
    }
    if depth > MAX_DEPTH {
        return Err(IndexError::Bounds);
    }

    let existing = match node {
        None => None,
        Some(child) => {
            let bytes = source
                .node(&child.hash)
                .ok_or(IndexError::MissingNode(child.hash))?;
            Some((child, IndexNode::decode_canonical(&bytes)?))
        }
    };

    match existing {
        // A fresh subtree, or one replacing a leaf: both resolve to "what is
        // the entry set here now?", so they share a path.
        None => {
            let entries: Vec<IndexEntry> = changes
                .iter()
                .filter_map(|c| {
                    c.value
                        .clone()
                        .map(|value| IndexEntry { key: c.key, value })
                })
                .collect();
            Ok(build(&entries, depth, sink))
        }
        Some((_, IndexNode::Leaf(entries))) => {
            let merged = merge_into(entries, changes);
            Ok(build(&merged, depth, sink))
        }
        Some((child, IndexNode::Branch(children))) => {
            let mut next = *children;
            let mut total = child.count;
            let mut start = 0usize;
            while start < changes.len() {
                let slot = nibble(&changes[start].key, depth);
                let mut end = start;
                while end < changes.len() && nibble(&changes[end].key, depth) == slot {
                    end += 1;
                }
                let before = next[slot].map_or(0, |c| c.count);
                let updated = descend(source, next[slot], depth + 1, &changes[start..end], sink)?;
                let after = updated.map_or(0, |c| c.count);
                total = total + after - before;
                next[slot] = updated;
                start = end;
            }

            if total == 0 {
                return Ok(None);
            }
            // The merge rule, stated as a property of the set: if everything
            // below this branch would fit one leaf, the canonical encoding of
            // that set *is* one leaf. Reading the subtree to find out is
            // bounded, because the counts already say it is small.
            if total <= MAX_LEAF_ENTRIES as u64 {
                let mut entries = Vec::with_capacity(total as usize);
                for slot in next.iter().flatten() {
                    collect(source, slot, depth + 1, &mut entries)?;
                }
                entries.sort_by(|a, b| a.key.cmp(&b.key));
                if fits_leaf(&entries, depth) {
                    return Ok(build(&entries, depth, sink));
                }
            }
            let node = IndexNode::Branch(Box::new(next));
            Ok(Some(sink.emit(&node, total)))
        }
    }
}

/// Fold a sorted change list into a sorted entry list.
fn merge_into(entries: Vec<IndexEntry>, changes: &[IndexChange]) -> Vec<IndexEntry> {
    let mut out: Vec<IndexEntry> = Vec::with_capacity(entries.len() + changes.len());
    let mut e = entries.into_iter().peekable();
    let mut c = changes.iter().peekable();
    loop {
        match (e.peek(), c.peek()) {
            (None, None) => break,
            (Some(_), None) => out.push(e.next().expect("peeked")),
            (None, Some(_)) => {
                let change = c.next().expect("peeked");
                if let Some(value) = change.value.clone() {
                    out.push(IndexEntry {
                        key: change.key,
                        value,
                    });
                }
            }
            (Some(entry), Some(change)) => match entry.key.cmp(&change.key) {
                std::cmp::Ordering::Less => out.push(e.next().expect("peeked")),
                std::cmp::Ordering::Greater => {
                    let change = c.next().expect("peeked");
                    if let Some(value) = change.value.clone() {
                        out.push(IndexEntry {
                            key: change.key,
                            value,
                        });
                    }
                }
                std::cmp::Ordering::Equal => {
                    e.next();
                    let change = c.next().expect("peeked");
                    if let Some(value) = change.value.clone() {
                        out.push(IndexEntry {
                            key: change.key,
                            value,
                        });
                    }
                }
            },
        }
    }
    out
}

/// Look one key up. O(depth) reads and no allocation past one node.
pub fn lookup(
    source: &dyn NodeSource,
    root: Option<ChildRef>,
    key: &IndexKey,
) -> Result<Option<Vec<u8>>, IndexError> {
    let mut current = root;
    let mut depth = 0usize;
    while let Some(child) = current {
        if depth > MAX_DEPTH {
            return Err(IndexError::Bounds);
        }
        let bytes = source
            .node(&child.hash)
            .ok_or(IndexError::MissingNode(child.hash))?;
        match IndexNode::decode_canonical(&bytes)? {
            IndexNode::Leaf(entries) => {
                return Ok(entries.into_iter().find(|e| &e.key == key).map(|e| e.value))
            }
            IndexNode::Branch(children) => {
                current = children[nibble(key, depth)];
                depth += 1;
            }
        }
    }
    Ok(None)
}

/// Walk the whole index in key order, calling `visit` per entry. Streaming:
/// one node is held at a time and no complete entry set is materialised.
pub fn stream(
    source: &dyn NodeSource,
    root: Option<ChildRef>,
    visit: &mut dyn FnMut(&IndexEntry),
) -> Result<u64, IndexError> {
    fn walk(
        source: &dyn NodeSource,
        child: &ChildRef,
        depth: usize,
        visit: &mut dyn FnMut(&IndexEntry),
    ) -> Result<u64, IndexError> {
        if depth > MAX_DEPTH {
            return Err(IndexError::Bounds);
        }
        let bytes = source
            .node(&child.hash)
            .ok_or(IndexError::MissingNode(child.hash))?;
        match IndexNode::decode_canonical(&bytes)? {
            IndexNode::Leaf(entries) => {
                for entry in &entries {
                    visit(entry);
                }
                Ok(entries.len() as u64)
            }
            IndexNode::Branch(children) => {
                let mut total = 0;
                for slot in children.iter().flatten() {
                    total += walk(source, slot, depth + 1, visit)?;
                }
                Ok(total)
            }
        }
    }
    match root {
        None => Ok(0),
        Some(child) => walk(source, &child, 0, visit),
    }
}

/// Every node hash reachable from a root — the structural spine. Small relative
/// to the entry set (one node per ~256 entries), which is what lets a sweep
/// probe entries by lookup instead of materialising them all.
pub fn spine(
    source: &dyn NodeSource,
    root: Option<ChildRef>,
) -> Result<std::collections::BTreeSet<[u8; 32]>, IndexError> {
    fn walk(
        source: &dyn NodeSource,
        child: &ChildRef,
        depth: usize,
        out: &mut std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<(), IndexError> {
        if depth > MAX_DEPTH {
            return Err(IndexError::Bounds);
        }
        if !out.insert(child.hash) {
            return Ok(());
        }
        let bytes = source
            .node(&child.hash)
            .ok_or(IndexError::MissingNode(child.hash))?;
        if let IndexNode::Branch(children) = IndexNode::decode_canonical(&bytes)? {
            for slot in children.iter().flatten() {
                walk(source, slot, depth + 1, out)?;
            }
        }
        Ok(())
    }
    let mut out = std::collections::BTreeSet::new();
    if let Some(child) = root {
        walk(source, &child, 0, &mut out)?;
    }
    Ok(out)
}

/// Full structural validation of an index a peer or a restart handed us:
/// canonical encoding, bounds, prefix placement, sorted unique keys, exact
/// counts, and canonical shape. Iterative bounds precede every allocation.
///
/// Canonical *shape* is the check worth naming. A tree can be internally
/// consistent — right hashes, right counts, keys where they belong — and still
/// not be the encoding this crate would produce for that set, because someone
/// split a leaf that fit or kept a branch that should have merged. Accepting
/// those would mean one set had many roots, and a root would stop being a
/// commitment to a set.
pub fn validate(source: &dyn NodeSource, root: Option<ChildRef>) -> Result<u64, IndexError> {
    fn walk(
        source: &dyn NodeSource,
        child: &ChildRef,
        depth: usize,
        prefix: &[usize],
    ) -> Result<u64, IndexError> {
        if depth > MAX_DEPTH {
            return Err(IndexError::Bounds);
        }
        let bytes = source
            .node(&child.hash)
            .ok_or(IndexError::MissingNode(child.hash))?;
        if node_hash(&bytes) != child.hash {
            return Err(IndexError::NonCanonical);
        }
        match IndexNode::decode_canonical(&bytes)? {
            IndexNode::Leaf(entries) => {
                if entries.is_empty() {
                    return Err(IndexError::NotCanonicalShape);
                }
                for w in entries.windows(2) {
                    if w[0].key >= w[1].key {
                        return Err(IndexError::Order);
                    }
                }
                for entry in &entries {
                    if entry.value.len() > MAX_VALUE_BYTES {
                        return Err(IndexError::Bounds);
                    }
                    for (level, expected) in prefix.iter().enumerate() {
                        if nibble(&entry.key, level) != *expected {
                            return Err(IndexError::Order);
                        }
                    }
                }
                if !fits_leaf(&entries, depth) {
                    return Err(IndexError::NotCanonicalShape);
                }
                if child.count != entries.len() as u64 {
                    return Err(IndexError::CountMismatch);
                }
                Ok(entries.len() as u64)
            }
            IndexNode::Branch(children) => {
                let occupied = children.iter().flatten().count();
                if occupied == 0 {
                    return Err(IndexError::NotCanonicalShape);
                }
                let mut total = 0u64;
                for (slot, entry) in children.iter().enumerate() {
                    let Some(reference) = entry else { continue };
                    let mut next = prefix.to_vec();
                    next.push(slot);
                    total += walk(source, reference, depth + 1, &next)?;
                }
                if total != child.count {
                    return Err(IndexError::CountMismatch);
                }
                // A branch whose whole subtree fits one leaf is not canonical:
                // the canonical encoding of that set is the leaf.
                if total <= MAX_LEAF_ENTRIES as u64 {
                    let mut entries = Vec::with_capacity(total as usize);
                    for slot in children.iter().flatten() {
                        collect(source, slot, depth + 1, &mut entries)?;
                    }
                    entries.sort_by(|a, b| a.key.cmp(&b.key));
                    if fits_leaf(&entries, depth) {
                        return Err(IndexError::NotCanonicalShape);
                    }
                }
                Ok(total)
            }
        }
    }
    match root {
        None => Ok(0),
        Some(child) => walk(source, &child, 0, &[]),
    }
}
