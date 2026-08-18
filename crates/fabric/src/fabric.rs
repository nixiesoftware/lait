//! The Engine operation surface and engine — the sealed contract Replica drives.
//!
//! Engine is LAIT's canonical, sealed Loro component and the only crate that
//! names Loro. It exposes **LAIT-owned** semantic operations and results, never
//! raw documents, containers, or Loro frontier types. Replica validates and
//! constructs a semantic transaction plan, submits it to a Engine-owned
//! [`Engine`] engine, and advances its semantic frontier only from a durable
//! [`Receipt`]. Engine never imports Replica.
//!
//! **Ownership boundary (enforced, not just documented):**
//! - Replica submits *semantic* [`Op`]s — it never authors a Loro delta.
//!   The concrete translation to Loro is Engine-private.
//! - [`Receipt`] and [`CausalToken`] can be constructed **only**
//!   inside this crate (their constructors are `pub(crate)`), so a receipt is
//!   proof of a real Engine commit — an outside crate cannot forge the token
//!   Replica advances from.
//!
//! [`Engine`] is the sole engine: atomic Bodies plus the frozen
//! collaborative algebra (register/map/list/text/set/counter with stable
//! element identity) over real Loro containers — one independent causal image
//! per Body, with only a bounded mutation-hot set inflated as live documents,
//! so per-Body export/import carries exactly one Body's history — with batch
//! atomicity. The durable store is the journaled protocol in
//! [`crate::journal`], persisting per-Body protected objects.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

/// An opaque commitment to Engine's internal causal position (Loro frontier),
/// carried as bytes. It rides inside [`Receipt`] and is never
/// interpreted outside Engine — no `loro::*` type crosses the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CausalToken(Vec<u8>);

impl CausalToken {
    /// Construct a causal token. **Crate-private**: only the Engine engine mints
    /// one, so a token always denotes a real Engine position.
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A key into the Engine representation — an opaque handle Replica uses to
/// address a durable object without naming a Loro container. Its concrete
/// encoding is Engine-private.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Key(Vec<u8>);

impl Key {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A single Engine-level **semantic** operation. Replica alone translates a
/// semantic `Op` into one of these; Engine maps them canonically onto Loro.
/// Replica never authors a raw Loro delta — that is the ownership boundary.
///
/// The collaborative operations implement the frozen S1 algebra: each addresses
/// a `path` inside one collaborative Body (`key`), a path is bound to exactly
/// one collaborative type for the Body's lifetime (a second type at the same
/// path is a [`Failure::TypeConflict`]), list elements carry **stable
/// element ids** minted by Engine at insert time (never indices), sets are
/// add-wins (observed-remove), counters sum all increments, and text splices
/// use Unicode-scalar coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// Atomically replace the canonical bytes stored at a key.
    PutCanonical { key: Key, value: Vec<u8> },
    /// Remove the object at a key (atomic value or whole collaborative Body).
    Remove { key: Key },
    /// Ensure a collaborative Body root exists at a key (Body create).
    CreateBody { key: Key },
    /// Last-writer-wins register set.
    RegisterSet {
        key: Key,
        path: String,
        value: Vec<u8>,
    },
    /// Clear a register.
    RegisterClear { key: Key, path: String },
    /// Map entry set (LWW per entry).
    MapSet {
        key: Key,
        path: String,
        entry: String,
        value: Vec<u8>,
    },
    /// Map entry remove.
    MapRemove {
        key: Key,
        path: String,
        entry: String,
    },
    /// Ordered-list insert at a position; Engine mints the stable element id.
    ListInsert {
        key: Key,
        path: String,
        index: u64,
        value: Vec<u8>,
    },
    /// Ordered-list remove **by stable element id**.
    ListRemove {
        key: Key,
        path: String,
        element: String,
    },
    /// Ordered-list move **by stable element id** to a position.
    ListMove {
        key: Key,
        path: String,
        element: String,
        index: u64,
    },
    /// Text splice with Unicode-scalar coordinates.
    TextSplice {
        key: Key,
        path: String,
        index: u64,
        delete: u64,
        insert: String,
    },
    /// Add-wins set add.
    SetAdd {
        key: Key,
        path: String,
        value: Vec<u8>,
    },
    /// Set remove (removes the observed adds; a concurrent add survives).
    SetRemove {
        key: Key,
        path: String,
        value: Vec<u8>,
    },
    /// Commutative counter increment.
    CounterAdd { key: Key, path: String, delta: i64 },
    /// Movable-hierarchy insert; Engine mints the stable node id.
    ///
    /// `parent` names the node this one hangs under — `None` makes it a root of
    /// the forest. `after` places it directly after that sibling, which must be
    /// a child of `parent`; `None` appends to the end of `parent`'s children.
    ///
    /// **Placement is local; parentage is not.** "The end of the children" is
    /// the end of the children *this replica can see*, and no sequence type can
    /// make it anything else — a replica fifty siblings behind appends fifty
    /// positions back, in a tree exactly as in a list. What converges is that
    /// every replica then agrees on the resulting order, that the node keeps
    /// the parent it named, and that concurrent inserts under one parent all
    /// survive. A caller that needs a chronology rather than a placement orders
    /// siblings by something the record carries, not by the sequence.
    TreeInsert {
        key: Key,
        path: String,
        parent: Option<String>,
        after: Option<String>,
        value: Vec<u8>,
    },
    /// Re-parent and/or re-place a node **by stable node id**. Concurrent moves
    /// converge on one hierarchy, and a move that would make a node its own
    /// ancestor is refused rather than resolved into a detached cycle.
    TreeMove {
        key: Key,
        path: String,
        node: String,
        parent: Option<String>,
        after: Option<String>,
    },
    /// Remove a node and, with it, its whole subtree.
    TreeRemove {
        key: Key,
        path: String,
        node: String,
    },
    /// Set one entry of a node's data map (LWW per entry, as for `map:`).
    TreeSet {
        key: Key,
        path: String,
        node: String,
        entry: String,
        value: Vec<u8>,
    },
    /// Remove one entry of a node's data map.
    TreeUnset {
        key: Key,
        path: String,
        node: String,
        entry: String,
    },
    /// Place the node carrying application anchor `anchor` under the one
    /// carrying `parent`, creating either if it does not exist yet. `parent:
    /// None` places it at a root of the forest.
    ///
    /// This exists because the other tree operations address a node by the id
    /// Engine minted for it, and **a batch cannot name a node it is itself
    /// creating** — the id does not exist until apply. That makes "file issue A
    /// under issue B" inexpressible as one atomic change when neither has a
    /// node yet, which is the ordinary case for a hierarchy over records that
    /// already exist. Addressing by the application's own key removes the
    /// ordering problem entirely: the operation is idempotent, needs no prior
    /// read, and one of them expresses the whole intent.
    ///
    /// Two replicas anchoring the same key concurrently each create a node, and
    /// the anchor then resolves to the lowest node id — the same one on every
    /// replica. The loser is left as an empty node at a root rather than merged
    /// away, because merging two nodes would have to merge their subtrees, and
    /// nothing here knows whether that is what the application meant.
    TreeAnchor {
        key: Key,
        path: String,
        anchor: String,
        parent: Option<String>,
    },
    /// Append to a log, keeping at most `retain` entries in state.
    ///
    /// The type exists because an append-only feed stored as a List makes every
    /// checkpoint carry every entry ever written: state *is* the whole feed, so
    /// a snapshot of an issue with ten thousand events is ten thousand events
    /// long, forever. A log's state is the retained tail plus an exact count of
    /// everything that ever arrived, so a checkpoint is bounded by `retain`
    /// while the count still answers "how many".
    ///
    /// Trimming deletes from the front of the converged order, which every
    /// replica sees identically, so replicas agree on what was dropped. Under
    /// concurrency the retained window is approximate — a replica trimming
    /// against a view that is missing another's appends drops entries that are
    /// not a prefix of the final order — while the count stays exact. `retain`
    /// is per-append rather than per-path because nothing here persists a
    /// setting; two replicas passing different values still converge, on the
    /// union of what either dropped.
    LogAppend {
        key: Key,
        path: String,
        value: Vec<u8>,
        retain: u64,
    },
}

/// A durable transaction request: an ordered batch of Engine operations to apply
/// atomically, carrying the request/commit metadata Engine labels the change
/// with in the oplog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    /// The semantic request label (e.g. `"created"`) surfaced in the oplog.
    pub request: String,
    pub ops: Vec<Op>,
}

impl Transaction {
    pub fn new(request: impl Into<String>, ops: Vec<Op>) -> Self {
        Self {
            request: request.into(),
            ops,
        }
    }
}

/// The receipt of a durable Engine commit. Replica advances its semantic
/// frontier **only** from this. It carries the post-commit causal token and the
/// count of changes applied. Constructed only by the Engine engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    causal: CausalToken,
    applied: u32,
}

impl Receipt {
    /// **Crate-private**: only the Engine engine issues a receipt.
    pub(crate) fn new(causal: CausalToken, applied: u32) -> Self {
        Self { causal, applied }
    }
    pub fn causal(&self) -> &CausalToken {
        &self.causal
    }
    pub fn applied(&self) -> u32 {
        self.applied
    }
}

/// Commit outcomes owned by the Engine boundary.
pub mod commit {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Invalid {
        Import,
        Bounds,
        ListIndex,
        UnknownElement,
        TextRange,
        CounterOverflow,
        Merge,
        /// A tree move would have made a node its own ancestor. Refused, not
        /// resolved: the alternative is a subtree that is reachable from
        /// nothing, which converges perfectly and is invisible to every reader.
        TreeCycle,
        /// A tree placement named a sibling that is not a child of the parent
        /// the same operation named. The two halves disagree about where the
        /// node goes, and picking one silently would honour a placement its
        /// writer did not ask for.
        TreePlacement,
        /// A tree data entry used a key Engine reserves for a node's own value.
        TreeReservedEntry,
    }

    /// Why an Engine commit failed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Failure {
        /// A durable write (or a rollback after a failed apply) failed. The engine
        /// state may have diverged from the store — the caller must fail stop.
        Journal(journal::Failure),
        /// The engine does not support this operation. Reserved (the Loro engine
        /// supports the full algebra); the error surface stays stable.
        Unsupported,
        /// The operation's type disagrees with what its target is already bound to:
        /// a collaborative op on an atomic Body, an atomic put over a collaborative
        /// Body, or a second collaborative type at an already-bound path.
        TypeConflict,
        /// The operation was structurally invalid at apply time (out-of-bounds
        /// index, unknown element id, counter overflow). The batch is rolled back.
        Invalid(Invalid),
        /// The authoritative switch happened but its durability confirmation
        /// failed: the commit may or may not survive power loss. Fail stop and
        /// reopen — recovery resolves the outcome deterministically from the
        /// on-disk manifest. Never retry through this error.
        OutcomeUnknown,
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for Failure {}

    impl From<journal::Failure> for Failure {
        fn from(e: journal::Failure) -> Self {
            match e {
                journal::Failure::OutcomeUnknown => Failure::OutcomeUnknown,
                failure => Failure::Journal(failure),
            }
        }
    }
}

use commit::Failure;

/// A canonical, Loro-free view of one collaborative Body, keyed by path. This
/// is what a World reads back through the bounded context: list elements expose
/// the **stable element ids** Engine minted at insert (the handles `ListRemove`
/// / `ListMove` take), sets expose distinct member values, counters the summed
/// total.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollaborativeView {
    pub registers: BTreeMap<String, Vec<u8>>,
    pub maps: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    pub lists: BTreeMap<String, Vec<ListElement>>,
    pub texts: BTreeMap<String, String>,
    /// Distinct member values, sorted (set order is not meaningful).
    pub sets: BTreeMap<String, Vec<Vec<u8>>>,
    pub counters: BTreeMap<String, i64>,
    /// Movable hierarchies, each flattened to pre-order: a node always appears
    /// after its parent, and siblings appear in the converged order.
    pub trees: BTreeMap<String, Vec<TreeNode>>,
    /// Append-only feeds: the retained tail, plus how many entries have ever
    /// been appended.
    pub logs: BTreeMap<String, LogView>,
}

/// One append-only feed as it reads back: the entries still retained, and the
/// exact total ever appended.
///
/// `appended` is not `entries.len()`. It counts everything that ever arrived,
/// including what trimming has since dropped from state, which is the whole
/// reason the type keeps a separate count — a reader that wants to say "1,247
/// events, here are the last 512" cannot get the first number from the second.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogView {
    pub entries: Vec<ListElement>,
    pub appended: u64,
}

/// One ordered-list element: its stable Engine-minted id and its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListElement {
    pub element: String,
    pub value: Vec<u8>,
}

/// One node of a movable hierarchy: the stable Engine-minted node id
/// `TreeMove`/`TreeRemove`/`TreeSet` take, the parent it hangs under (`None`
/// at a root of the forest), the value its insert carried, and the data
/// entries set on it since.
///
/// Deleted nodes are absent, and so are their descendants: a subtree removal
/// takes the subtree with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeNode {
    pub node: String,
    pub parent: Option<String>,
    pub value: Vec<u8>,
    pub entries: BTreeMap<String, Vec<u8>>,
    /// The application key this node answers to, when it was placed by
    /// [`Op::TreeAnchor`] rather than inserted directly.
    pub anchor: Option<String>,
}

/// Decode the `element_id[16] || value` blobs a sequence root holds. Shared by
/// `list:` and `log:`, which store entries identically — a log entry is a list
/// element that the type has agreed to stop keeping.
fn list_elements(items: &[loro::LoroValue]) -> Vec<ListElement> {
    let mut elements = Vec::new();
    for v in items {
        let loro::LoroValue::Binary(bytes) = v else {
            continue;
        };
        if bytes.len() < ELEMENT_ID_LEN {
            continue;
        }
        let Some((identity, value)) = bytes.split_at_checked(ELEMENT_ID_LEN) else {
            continue;
        };
        elements.push(ListElement {
            element: data_encoding::HEXLOWER.encode(identity),
            value: value.to_vec(),
        });
    }
    elements
}

/// Flatten one level of a tree's projected value into pre-order, recursing into
/// each node's children. `false` means a node was not shaped like a node, which
/// the caller reports as a malformed Body rather than skipping — a hierarchy
/// missing an interior node is missing everything below it, and a reader
/// handed the remainder cannot tell.
///
/// Loro projects a tree as a nested array of `{id, parent, meta, index,
/// fractional_index, children}`, already in sibling order, with `meta` resolved
/// because the whole-doc read is a *deep* value. Parentage comes from the
/// recursion rather than from the `parent` field: they agree, and the one that
/// cannot disagree is the one the walk itself establishes.
fn flatten_tree(nodes: &[loro::LoroValue], parent: Option<&str>, out: &mut Vec<TreeNode>) -> bool {
    for node in nodes {
        let loro::LoroValue::Map(fields) = node else {
            return false;
        };
        let Some(loro::LoroValue::String(id)) = fields.get("id") else {
            return false;
        };
        let mut value = Vec::new();
        let mut anchor = None;
        let mut entries = BTreeMap::new();
        // A node whose meta has never been written projects as an empty map,
        // and a node created by a peer that wrote no value is a node with no
        // value — both are ordinary, neither is malformed.
        if let Some(loro::LoroValue::Map(meta)) = fields.get("meta") {
            for (k, v) in meta.iter() {
                let loro::LoroValue::Binary(bytes) = v else {
                    continue;
                };
                if k == NODE_VALUE_KEY {
                    value = bytes.to_vec();
                } else if k == NODE_ANCHOR_KEY {
                    anchor = String::from_utf8(bytes.to_vec()).ok();
                } else {
                    entries.insert(k.clone(), bytes.to_vec());
                }
            }
        }
        let id = id.to_string();
        out.push(TreeNode {
            node: id.clone(),
            parent: parent.map(str::to_string),
            value,
            entries,
            anchor,
        });
        match fields.get("children") {
            Some(loro::LoroValue::List(children)) => {
                if !flatten_tree(children, Some(&id), out) {
                    return false;
                }
            }
            // A leaf may carry no `children` at all; anything else there is not
            // a hierarchy.
            None | Some(loro::LoroValue::Null) => {}
            Some(_) => return false,
        }
    }
    true
}

/// The Body identity an anchor carries. A digest rather than the key itself so
/// an anchor stays fixed-size whatever a caller names its Bodies.
fn body_digest(key: &Key) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"lait/fabric-anchor-body/1");
    h.update(key.as_bytes());
    *h.finalize().as_bytes()
}

/// One Body's canonical exported representation: an atomic Body's canonical
/// application bytes, or a collaborative Body's canonical per-Body Engine
/// export (causality and stable element identity preserved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyExport {
    Atomic(Vec<u8>),
    Collaborative(Vec<u8>),
}

/// An immutable, thread-safe read image of one Body.
///
/// A live [`Engine`] owns non-`Sync` collaborative documents and therefore
/// cannot sit behind a shared reader lock. This value crosses that boundary as
/// plain canonical data. Collaborative projection is decoded only when a
/// caller explicitly reads this Body and is not retained beside the canonical
/// export. The compact causal version is memoized independently, so write
/// routing can ask for it without keeping a full view or live `LoroDoc`.
#[derive(Debug, Clone)]
pub struct BodySnapshot {
    export: SnapshotExport,
    /// Atomic Bodies have the known empty causal version and therefore need
    /// no heap cell. Collaborative snapshots share the lazy/verified cell
    /// with their frozen Engine state.
    version: Option<Arc<OnceLock<Result<crate::causal::Version, crate::causal::Invalid>>>>,
}

#[derive(Debug, Clone)]
enum SnapshotExport {
    Atomic(Arc<[u8]>),
    Collaborative(Arc<[u8]>),
}

impl BodySnapshot {
    /// Canonical Body bytes retained by this immutable image. This is an O(1)
    /// sizing seam for publication/cursor admission; callers need not decode
    /// collaborative material merely to price its residency.
    pub fn retained_bytes(&self) -> u64 {
        let len = match &self.export {
            SnapshotExport::Atomic(bytes) | SnapshotExport::Collaborative(bytes) => bytes.len(),
        };
        u64::try_from(len).unwrap_or(u64::MAX)
    }

    /// Freeze one canonical Body export. The key is required because anchors
    /// bind to a Body identity even though the export itself is per-Body.
    pub fn from_export(_key: &Key, export: BodyExport) -> Result<Self, Failure> {
        Ok(match export {
            BodyExport::Atomic(bytes) => Self::from_atomic(bytes.into()),
            BodyExport::Collaborative(bytes) => Self {
                export: SnapshotExport::Collaborative(bytes.into()),
                version: Some(Arc::new(OnceLock::new())),
            },
        })
    }

    fn from_atomic(export: Arc<[u8]>) -> Self {
        Self {
            export: SnapshotExport::Atomic(export),
            version: None,
        }
    }

    fn from_frozen(frozen: &FrozenCollab) -> Self {
        Self {
            export: SnapshotExport::Collaborative(frozen.export.clone()),
            version: Some(frozen.version.clone()),
        }
    }

    pub fn read(&self) -> Option<Vec<u8>> {
        match &self.export {
            SnapshotExport::Atomic(bytes) => Some(bytes.to_vec()),
            SnapshotExport::Collaborative(_) => None,
        }
    }

    /// Share the exact immutable Atomic bytes without copying them.
    ///
    /// A caller that obtains this from a governed cold-image cache must keep
    /// its cache pin/lease alive for at least as long as the returned `Arc`.
    /// Resident publication snapshots need no second lease because the
    /// publication itself already accounts for and pins their export.
    pub fn read_shared(&self) -> Option<Arc<[u8]>> {
        match &self.export {
            SnapshotExport::Atomic(bytes) => Some(bytes.clone()),
            SnapshotExport::Collaborative(_) => None,
        }
    }

    /// Share the exact canonical retained export for either mutation model.
    /// Frozen collaborative images return the same `Arc` held by the Engine;
    /// this never imports, projects, serializes, or copies their Loro state.
    pub fn canonical_export_shared(&self) -> Arc<[u8]> {
        match &self.export {
            SnapshotExport::Atomic(bytes) | SnapshotExport::Collaborative(bytes) => bytes.clone(),
        }
    }

    pub fn read_collaborative(&self) -> Result<CollaborativeView, ProjectionFailure> {
        let SnapshotExport::Collaborative(bytes) = &self.export else {
            return Err(ProjectionFailure::NotCollaborative);
        };
        let doc =
            import_collaborative_doc(bytes, None).map_err(|_| ProjectionFailure::Malformed)?;
        project_collaborative_doc(&doc)
    }

    pub fn version(&self) -> Result<crate::causal::Version, crate::causal::Invalid> {
        let SnapshotExport::Collaborative(bytes) = &self.export else {
            return Ok(crate::causal::Version::empty());
        };
        let version = self
            .version
            .as_ref()
            .ok_or(crate::causal::Invalid::Engine)?;
        version
            .get_or_init(|| {
                import_collaborative_doc(bytes, None)
                    .map(|doc| crate::causal::Version::from_frontiers(&doc.oplog_frontiers()))
                    .map_err(|_| crate::causal::Invalid::Engine)
            })
            .clone()
    }

    /// Mint an anchor without borrowing the live writer. Anchor traffic is
    /// sparse; importing one retained Body here is preferable to making every
    /// query serialize behind the writer merely because some queries can ask
    /// for an anchor.
    /// Mint an anchor from this exact collaborative image while preserving the
    /// distinction between a model mismatch, a schema this build cannot
    /// project, and malformed retained material.
    pub fn try_anchor(
        &self,
        key: &Key,
        path: &str,
        position: u64,
    ) -> Result<crate::causal::Anchor, ProjectionFailure> {
        let SnapshotExport::Collaborative(bytes) = &self.export else {
            return Err(ProjectionFailure::NotCollaborative);
        };
        let doc =
            import_collaborative_doc(bytes, None).map_err(|_| ProjectionFailure::Malformed)?;
        // Validate the declared collaborative root vocabulary before minting
        // a position from it. An unknown root is schema-ahead, not corruption
        // and not an absent Body.
        project_collaborative_doc(&doc)?;
        anchor_in_doc(&doc, key, path, position).map_err(|_| ProjectionFailure::Malformed)
    }

    pub fn anchor(&self, key: &Key, path: &str, position: u64) -> Option<crate::causal::Anchor> {
        self.try_anchor(key, path, position).ok()
    }

    /// Resolve an anchor against this exact collaborative image without
    /// turning an import/schema failure into ordinary positional drift.
    pub fn try_resolve(
        &self,
        key: &Key,
        anchor: &crate::causal::Anchor,
    ) -> Result<crate::causal::AnchorResolution, ProjectionFailure> {
        let SnapshotExport::Collaborative(bytes) = &self.export else {
            return Err(ProjectionFailure::NotCollaborative);
        };
        let doc =
            import_collaborative_doc(bytes, None).map_err(|_| ProjectionFailure::Malformed)?;
        project_collaborative_doc(&doc)?;
        Ok(resolve_in_doc(&doc, key, anchor))
    }

    pub fn resolve(
        &self,
        key: &Key,
        anchor: &crate::causal::Anchor,
    ) -> crate::causal::AnchorResolution {
        self.try_resolve(key, anchor)
            .unwrap_or(crate::causal::AnchorResolution::Drifted)
    }

    pub fn export(&self) -> BodyExport {
        match &self.export {
            SnapshotExport::Atomic(bytes) => BodyExport::Atomic(bytes.to_vec()),
            SnapshotExport::Collaborative(bytes) => BodyExport::Collaborative(bytes.to_vec()),
        }
    }
}

/// The Loro-backed engine: the canonical collaborative representation, and the
/// reason this crate alone names Loro.
///
/// **Layout — one causal image per Body, bounded live documents.** Each
/// collaborative Body has its own canonical export, so
/// ([`fabric::export_body`]) carries exactly that Body's causal history and
/// stable element identity — never a whole-engine or cross-Body snapshot. Cold
/// images remain Arc-backed export bytes plus a compact Version; mutation
/// inflates at most [`MAX_HOT_COLLABORATIVE_BODIES`] `LoroDoc`s and LRU eviction
/// freezes them again. Inside an inflated Body, one root map (`body`)
/// holds keys `"<type>:<path>"` — `reg:` LWW binary registers, `map:` child
/// maps of binary entries, `list:` child movable lists whose element values are
/// `element_id[16] || value` (the id embedded in the value is the **stable
/// element identity**, LAIT-owned and sync-stable), `text:` child texts
/// (Unicode-scalar splices), `set:` child maps implementing an observed-remove
/// set (`"<value-hash>:<unique-tag>"` per add, so a remove only deletes the
/// adds it observed — add-wins), `cnt:` child maps implementing a
/// PN-counter (each doc session sums into its own peer key; concurrent
/// increments land in disjoint keys and always sum), and `tree:` child movable
/// trees whose nodes carry a data map each, the node's own value living at the
/// reserved entry [`NODE_VALUE_KEY`]. An atomic Body is a plain
/// canonical byte value — its export is the application bytes themselves, and
/// replacement policy for concurrent atomic writes is decided by Replica, not
/// here.
///
/// **Atomicity.** A batch backs up every Body it touches before applying; any
/// apply error restores exactly those Bodies, so a failed batch changes
/// nothing. The receipt's causal token digests the touched Bodies' post-commit
/// positions.
///
/// **Concurrent path creation.** Every typed path resolves to a ROOT
/// container named `<tag>:<path>` (registers are keys in the `body` root
/// map). Root containers are identified by NAME, so two peers creating the
/// same fresh path concurrently address the SAME container and their edits
/// merge — the child-container/LWW shadowing that op-identified containers
/// suffer under concurrency cannot occur (the multi-writer reference corpus
/// proved that shadowing fatal for a shared catalog).
pub struct Engine {
    /// This activation's writer id, minted once and shared by every document
    /// this engine authors into. One id per activation rather than one per
    /// document keeps a Body's version vector growing with restarts rather than
    /// with Bodies, and never persisting it is what keeps a copied store from
    /// minting colliding operation ids.
    writer: u64,
    bodies: BTreeMap<Key, BodyState>,
    /// Mutation-hot collaborative documents. Imported/read-only Bodies remain
    /// compact exports; this bounded recency set prevents a broad sequence of
    /// writes from turning the whole store back into live `LoroDoc`s.
    hot: std::collections::VecDeque<Key>,
    /// Durable Replica owns the authoritative causal closure for cold Bodies.
    /// In that mode an LRU eviction drops the frozen export instead of keeping
    /// a second unbounded semantic store beside the Journal/read publication.
    external_collaborative_images: bool,
    /// Bodies displaced while a transaction is being prepared. They stay as
    /// frozen exports until finalize/rollback so a batch touching more than
    /// the hot cap can still export every touched Body atomically.
    external_evicted: std::collections::VecDeque<Key>,
}

/// One Body's live state.
enum BodyState {
    Atomic(Arc<[u8]>),
    Collab(loro::LoroDoc),
    FrozenCollab(FrozenCollab),
}

#[derive(Clone)]
struct FrozenCollab {
    export: Arc<[u8]>,
    version: Arc<OnceLock<Result<crate::causal::Version, crate::causal::Invalid>>>,
}

const MAX_HOT_COLLABORATIVE_BODIES: usize = 64;

/// One applied but unpublished Engine transaction.
///
/// The live Engine exposes the candidate values while this token exists. The
/// owner must either [`Engine::finalize`] it after the enclosing durable
/// publication succeeds or [`Engine::rollback`] it when candidate validation
/// fails. Its rollback state is proportional only to touched Bodies; a
/// collaborative `LoroDoc` clone shares its underlying document.
pub struct Prepared {
    receipt: Receipt,
    prior: BTreeMap<Key, Option<(BodyState, Option<loro::Frontiers>)>>,
}

/// A cheap, immutable coordinate for building an ordinary checkpoint away
/// from the publication path.
///
/// A mutation-hot document shares its backing history and captures only its
/// frontier. A cold document shares its already-retained export and version
/// memo. Importing that export is deliberately deferred to [`Self::export`],
/// which the checkpoint executor calls only after reserving bounded worker
/// capacity. Capturing a seed therefore never turns a cold Body back into a
/// live `LoroDoc` on the user-action or Contact path.
pub enum CheckpointSeed {
    Hot {
        doc: loro::LoroDoc,
        frontier: loro::Frontiers,
    },
    Cold {
        export: Arc<[u8]>,
        version: Arc<OnceLock<Result<crate::causal::Version, crate::causal::Invalid>>>,
    },
}

impl CheckpointSeed {
    pub fn export(self) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
        let (result, bytes) = match self {
            Self::Hot { doc, frontier } => {
                let doc = doc.fork_at(&frontier).map_err(|source| {
                    tracing::warn!(%source, "fabric checkpoint fork failed");
                    crate::causal::Invalid::Engine
                })?;
                let bytes = doc.export(loro::ExportMode::Snapshot).map_err(|source| {
                    tracing::warn!(%source, "fabric detached checkpoint export failed");
                    crate::causal::Invalid::Engine
                })?;
                (crate::causal::Version::from_frontiers(&frontier), bytes)
            }
            Self::Cold { export, version } => {
                // Even discovering an uncached cold version requires a Loro
                // import. Keep that work here, after the executor reservation,
                // rather than in `Engine::checkpoint_seed`.
                let result = if let Some(version) = version.get() {
                    version.clone()?
                } else {
                    let doc = import_collaborative_doc(&export, None)
                        .map_err(|_| crate::causal::Invalid::Engine)?;
                    let discovered = crate::causal::Version::from_frontiers(&doc.oplog_frontiers());
                    let _ = version.set(Ok(discovered.clone()));
                    discovered
                };
                (result, export.to_vec())
            }
        };
        Ok(crate::causal::Artifact::Checkpoint {
            format_version: crate::causal::CAUSAL_FORMAT_VERSION,
            retention_frontier: crate::causal::Version::empty(),
            result,
            bytes,
        })
    }
}

impl Prepared {
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }
}

const BODY_MAP: &str = "body";

/// Domain for the receipt's causal token digest.
const CAUSAL_DOMAIN: &[u8] = b"lait/fabric-causal/1";

/// The collaborative type tags a path can be bound to.
const TYPE_TAGS: [&str; 8] = ["reg", "map", "list", "text", "set", "cnt", "tree", "log"];

/// Type tags reserved for collaborative types a later docket adds. Empty, and
/// that is a statement rather than an oversight: both reservations were spent
/// on the types the product was working around, `tree` and `log`, and both are
/// implemented. A ninth type is now a `CollaborativeSchema` version bump plus a
/// fixture set for an *unreserved* tag, which is the expensive path this list
/// existed to avoid — so the next type to be foreseen belongs here before the
/// encoding that would have to migrate around it ships.
const RESERVED_TYPE_TAGS: [&str; 0] = [];

/// The companion root a `log:` path binds alongside its entries: the exact
/// count of everything ever appended, as a PN-counter.
///
/// The count cannot live in the entry list, because the whole point of the type
/// is that the list does not keep everything. It cannot be derived from the
/// list either, for the same reason. And it cannot be one number in a register,
/// because two replicas appending concurrently would each write "mine plus
/// what I saw" and one would win — the count would then under-report by
/// exactly the concurrency the type exists to survive. Per-peer sums always
/// add up.
const LOG_COUNT_TAG: &str = "logn";

/// The node data entry a node's own value occupies.
///
/// Reserved rather than conventional: `TreeInsert` carries a value because a
/// writer cannot name the node id Engine is about to mint, so create-with-data
/// has to be one operation. That value needs somewhere to live inside the
/// node's data map, and a caller that could also write there would be able to
/// overwrite a node's payload while believing it set a field of its own. The
/// leading NUL keeps it outside the entry space any caller can name — `TreeSet`
/// refuses it — and outside the projection's `entries`, which reports the
/// caller's own keys only.
pub const NODE_VALUE_KEY: &str = "\u{0}value";

/// The node data entry an application anchor occupies, reserved for the same
/// reason as [`NODE_VALUE_KEY`]: a caller that could write it could re-point
/// another record's node at its own key.
pub const NODE_ANCHOR_KEY: &str = "\u{0}anchor";

/// Projection outcomes owned by the collaborative read boundary.
pub mod projection {
    /// Why a collaborative projection could not be produced.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Failure {
        /// No Body at this key, or the Body is atomic rather than collaborative.
        NotCollaborative,
        /// The Body binds a path to a collaborative type this build does not
        /// implement. Reserved-but-unimplemented tags land here, which is the
        /// point: schema gating upstream should have refused the Body already, and
        /// the projection layer is the wrong place to paper over that.
        SchemaAhead,
        /// A known tag bound to a value shape it cannot hold. Corruption or a
        /// schema disagreement, not a version gap.
        Malformed,
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Failure::NotCollaborative => write!(f, "not a collaborative Body"),
                Failure::SchemaAhead => write!(f, "collaborative schema is ahead of this build"),
                Failure::Malformed => write!(f, "collaborative material is malformed"),
            }
        }
    }
    impl std::error::Error for Failure {}
}

use projection::Failure as ProjectionFailure;

/// Whether a tag names a collaborative type this build implements.
pub fn is_implemented_type_tag(tag: &str) -> bool {
    TYPE_TAGS.contains(&tag)
}

/// Whether a tag is reserved for a future collaborative type.
pub fn is_reserved_type_tag(tag: &str) -> bool {
    RESERVED_TYPE_TAGS.contains(&tag)
}

/// A list element id is 16 minted bytes, rendered as 32 hex chars.
const ELEMENT_ID_LEN: usize = 16;

fn typed_key(tag: &str, path: &str) -> String {
    format!("{tag}:{path}")
}

fn mint_bytes<const N: usize>() -> [u8; N] {
    let mut raw = [0u8; N];
    crate::op::fill_identity(&mut raw);
    raw
}

/// The set-member key prefix for a value: 128 bits of BLAKE3 over the value.
fn set_member_prefix(value: &[u8]) -> String {
    let digest = blake3::hash(value);
    let prefix = digest.as_bytes().get(..16).unwrap_or(digest.as_bytes());
    data_encoding::HEXLOWER.encode(prefix)
}

/// A fresh per-Body doc with the crate's canonical Loro config, authoring as
/// `writer`.
fn new_body_doc(writer: Option<u64>) -> loro::LoroDoc {
    let doc = loro::LoroDoc::new();
    crate::op::configure(&doc, writer);
    doc
}

fn import_collaborative_doc(
    snapshot: &[u8],
    writer: Option<u64>,
) -> Result<loro::LoroDoc, Failure> {
    let doc = new_body_doc(writer);
    doc.import(snapshot)
        .map_err(|_| Failure::Invalid(commit::Invalid::Import))?;
    Ok(doc)
}

fn project_collaborative_doc(doc: &loro::LoroDoc) -> Result<CollaborativeView, ProjectionFailure> {
    let mut view = CollaborativeView::default();
    let loro::LoroValue::Map(roots) = doc.get_deep_value() else {
        return Ok(view);
    };
    for (name, value) in roots.iter() {
        if name == BODY_MAP {
            let loro::LoroValue::Map(body) = value else {
                continue;
            };
            for (key, value) in body.iter() {
                if let (Some(path), loro::LoroValue::Binary(bytes)) =
                    (key.strip_prefix("reg:"), value)
                {
                    view.registers.insert(path.to_string(), bytes.to_vec());
                }
            }
            continue;
        }
        let Some((tag, path)) = name.split_once(':') else {
            tracing::warn!(tag = %name, "unrecognized collaborative root");
            return Err(ProjectionFailure::SchemaAhead);
        };
        if tag == LOG_COUNT_TAG {
            let loro::LoroValue::Map(counts) = value else {
                tracing::warn!(tag, "log count held an invalid value");
                return Err(ProjectionFailure::Malformed);
            };
            let total = counts
                .values()
                .filter_map(|value| match value {
                    loro::LoroValue::I64(value) => u64::try_from(*value).ok(),
                    _ => None,
                })
                .fold(0u64, u64::saturating_add);
            view.logs.entry(path.to_string()).or_default().appended = total;
            continue;
        }
        if !is_implemented_type_tag(tag) {
            tracing::warn!(tag, "unsupported collaborative type");
            return Err(ProjectionFailure::SchemaAhead);
        }
        let path = path.to_string();
        match (tag, value) {
            ("map", loro::LoroValue::Map(map)) => {
                let mut entries = BTreeMap::new();
                for (key, value) in map.iter() {
                    if let loro::LoroValue::Binary(bytes) = value {
                        entries.insert(key.clone(), bytes.to_vec());
                    }
                }
                view.maps.insert(path, entries);
            }
            ("list", loro::LoroValue::List(items)) => {
                view.lists.insert(path, list_elements(items));
            }
            ("log", loro::LoroValue::List(items)) => {
                view.logs.entry(path).or_default().entries = list_elements(items);
            }
            ("text", loro::LoroValue::String(text)) => {
                view.texts.insert(path, text.to_string());
            }
            ("set", loro::LoroValue::Map(map)) => {
                let mut members: Vec<Vec<u8>> = map
                    .values()
                    .filter_map(|value| match value {
                        loro::LoroValue::Binary(bytes) => Some(bytes.to_vec()),
                        _ => None,
                    })
                    .collect();
                members.sort();
                members.dedup();
                view.sets.insert(path, members);
            }
            ("tree", loro::LoroValue::List(roots)) => {
                let mut nodes = Vec::new();
                if !flatten_tree(roots, None, &mut nodes) {
                    tracing::warn!(tag, "tree node held an invalid value");
                    return Err(ProjectionFailure::Malformed);
                }
                view.trees.insert(path, nodes);
            }
            ("cnt", loro::LoroValue::Map(map)) => {
                let total = map
                    .values()
                    .filter_map(|value| match value {
                        loro::LoroValue::I64(value) => Some(*value),
                        _ => None,
                    })
                    .fold(0i64, i64::saturating_add);
                view.counters.insert(path, total);
            }
            _ => {
                tracing::warn!(tag, "collaborative type held an invalid value");
                return Err(ProjectionFailure::Malformed);
            }
        }
    }
    Ok(view)
}

fn anchor_in_doc(
    doc: &loro::LoroDoc,
    key: &Key,
    path: &str,
    position: u64,
) -> Result<crate::causal::Anchor, crate::causal::Invalid> {
    let text = doc.get_text(typed_key("text", path));
    let length = u64::try_from(text.len_unicode()).unwrap_or(u64::MAX);
    let anchored_to = if position == 0 || length == 0 {
        None
    } else {
        text.get_cursor(
            usize::try_from(position.min(length))
                .unwrap_or(usize::MAX)
                .saturating_sub(1),
            loro::cursor::Side::Right,
        )
        .and_then(|cursor| cursor.id)
        .map(|id| crate::causal::OpHead {
            writer: id.peer,
            sequence: id.counter,
        })
    };
    Ok(crate::causal::Anchor {
        format_version: crate::causal::CAUSAL_FORMAT_VERSION,
        body: body_digest(key),
        path: path.to_string(),
        anchored_to,
        offset: position,
        after: true,
        taken_at: crate::causal::Version::from_frontiers(&doc.oplog_frontiers()),
    })
}

fn resolve_in_doc(
    doc: &loro::LoroDoc,
    key: &Key,
    anchor: &crate::causal::Anchor,
) -> crate::causal::AnchorResolution {
    use crate::causal::AnchorResolution;
    if anchor.body != body_digest(key) {
        return AnchorResolution::Drifted;
    }
    let text = doc.get_text(typed_key("text", &anchor.path));
    let length = u64::try_from(text.len_unicode()).unwrap_or(u64::MAX);
    let Some(head) = anchor.anchored_to else {
        return AnchorResolution::Resolved(anchor.offset.min(length));
    };
    let cursor = loro::cursor::Cursor::new(
        Some(loro::ID {
            peer: head.writer,
            counter: head.sequence,
        }),
        loro::ContainerID::Root {
            name: typed_key("text", &anchor.path).into(),
            container_type: loro::ContainerType::Text,
        },
        if anchor.after {
            loro::cursor::Side::Right
        } else {
            loro::cursor::Side::Left
        },
        usize::try_from(anchor.offset).unwrap_or(usize::MAX),
    );
    match doc.get_cursor_pos(&cursor) {
        Ok(position) if position.current.side == loro::cursor::Side::Right => {
            let resolved = u64::try_from(position.current.pos)
                .unwrap_or(u64::MAX)
                .saturating_add(1)
                .min(length);
            AnchorResolution::Resolved(resolved)
        }
        Ok(_) | Err(_) => AnchorResolution::Drifted,
    }
}

fn export_delta_from_doc(
    doc: &loro::LoroDoc,
    from: &crate::causal::Version,
) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
    let base = from.to_frontiers();
    let Some(version_vector) = doc.frontiers_to_vv(&base) else {
        return Err(crate::causal::Invalid::MissingBase);
    };
    let bytes = doc
        .export(loro::ExportMode::updates(&version_vector))
        .map_err(|source| {
            tracing::warn!(%source, "fabric delta export failed");
            crate::causal::Invalid::Engine
        })?;
    Ok(crate::causal::Artifact::Delta {
        format_version: crate::causal::CAUSAL_FORMAT_VERSION,
        base: from.clone(),
        result: crate::causal::Version::from_frontiers(&doc.oplog_frontiers()),
        bytes,
    })
}

fn export_checkpoint_from_doc(
    doc: &loro::LoroDoc,
    retention_frontier: &crate::causal::Version,
) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
    let (frontier, bytes) = if retention_frontier.is_empty() {
        let bytes = doc.export(loro::ExportMode::Snapshot).map_err(|source| {
            tracing::warn!(%source, "fabric checkpoint export failed");
            crate::causal::Invalid::Engine
        })?;
        (loro::Frontiers::default(), bytes)
    } else {
        let frontier = retention_frontier.to_frontiers();
        if doc.frontiers_to_vv(&frontier).is_none() {
            return Err(crate::causal::Invalid::MissingBase);
        }
        let bytes = doc
            .export(loro::ExportMode::shallow_snapshot(&frontier))
            .map_err(|source| {
                tracing::warn!(%source, "fabric compaction checkpoint export failed");
                crate::causal::Invalid::Engine
            })?;
        (frontier, bytes)
    };
    Ok(crate::causal::Artifact::Checkpoint {
        format_version: crate::causal::CAUSAL_FORMAT_VERSION,
        retention_frontier: crate::causal::Version::from_frontiers(&frontier),
        result: crate::causal::Version::from_frontiers(&doc.oplog_frontiers()),
        bytes,
    })
}

fn export_history_from_doc(
    doc: &loro::LoroDoc,
) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
    let bytes = doc.export(loro::ExportMode::Snapshot).map_err(|source| {
        tracing::warn!(%source, "fabric history export failed");
        crate::causal::Invalid::Engine
    })?;
    Ok(crate::causal::Artifact::Archive {
        format_version: crate::causal::CAUSAL_FORMAT_VERSION,
        result: crate::causal::Version::from_frontiers(&doc.oplog_frontiers()),
        bytes,
    })
}

impl BodyState {
    /// A position digest for the causal token: the atomic bytes' hash, or the
    /// collaborative doc's oplog frontier.
    fn digest(&self) -> Result<Vec<u8>, Failure> {
        match self {
            BodyState::Atomic(bytes) => Ok(blake3::hash(bytes).as_bytes().to_vec()),
            BodyState::Collab(doc) => Ok(doc.oplog_frontiers().encode().to_vec()),
            BodyState::FrozenCollab(frozen) => frozen
                .version()
                .map(|version| version.to_frontiers().encode().to_vec()),
        }
    }

    fn export(&self) -> Result<BodyExport, Failure> {
        match self {
            BodyState::Atomic(bytes) => Ok(BodyExport::Atomic(bytes.to_vec())),
            BodyState::Collab(doc) => doc
                .export(loro::ExportMode::Snapshot)
                .map(BodyExport::Collaborative)
                .map_err(|_| Failure::Invalid(commit::Invalid::Import)),
            BodyState::FrozenCollab(frozen) => {
                Ok(BodyExport::Collaborative(frozen.export.to_vec()))
            }
        }
    }
}

impl FrozenCollab {
    fn new(export: Arc<[u8]>) -> Self {
        Self {
            export,
            version: Arc::new(OnceLock::new()),
        }
    }

    fn from_doc(doc: &loro::LoroDoc) -> Result<Self, Failure> {
        let bytes = doc
            .export(loro::ExportMode::Snapshot)
            .map_err(|_| Failure::Invalid(commit::Invalid::Import))?;
        let version = Arc::new(OnceLock::new());
        let _ = version.set(Ok(crate::causal::Version::from_frontiers(
            &doc.oplog_frontiers(),
        )));
        Ok(Self {
            export: bytes.into(),
            version,
        })
    }

    fn doc(&self, writer: Option<u64>) -> Result<loro::LoroDoc, Failure> {
        import_collaborative_doc(&self.export, writer)
    }

    fn version(&self) -> Result<crate::causal::Version, Failure> {
        if let Some(version) = self.version.get() {
            return version
                .clone()
                .map_err(|_| Failure::Invalid(commit::Invalid::Import));
        }
        let doc = self.doc(None)?;
        let version = crate::causal::Version::from_frontiers(&doc.oplog_frontiers());
        let _ = self.version.set(Ok(version.clone()));
        Ok(version)
    }
}

impl Engine {
    /// Capture a Body position for off-path ordinary checkpoint construction.
    /// This call performs no snapshot serialization or history copy.
    pub fn checkpoint_seed(&self, key: &Key) -> Result<CheckpointSeed, crate::causal::Invalid> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => Ok(CheckpointSeed::Hot {
                doc: doc.clone(),
                frontier: doc.oplog_frontiers(),
            }),
            Some(BodyState::FrozenCollab(frozen)) => Ok(CheckpointSeed::Cold {
                export: Arc::clone(&frozen.export),
                version: Arc::clone(&frozen.version),
            }),
            _ => Err(crate::causal::Invalid::NotCollaborative),
        }
    }

    /// A fresh, empty Loro-backed engine, minting this activation's writer id.
    pub fn new() -> Self {
        Self {
            writer: crate::op::mint_activation_peer(),
            bodies: BTreeMap::new(),
            hot: std::collections::VecDeque::new(),
            external_collaborative_images: false,
            external_evicted: std::collections::VecDeque::new(),
        }
    }

    /// Declare that durable causal material outside this Engine can restore a
    /// collaborative Body on demand. At most the mutation-hot LRU remains in
    /// memory; evicted writers are removed rather than frozen indefinitely.
    pub fn use_external_collaborative_images(&mut self) {
        self.external_collaborative_images = true;
    }

    /// This activation's writer id.
    pub fn writer(&self) -> u64 {
        self.writer
    }

    /// The keys of every present Body.
    pub fn body_keys(&self) -> Vec<Key> {
        self.bodies.keys().cloned().collect()
    }

    /// Number of retained Bodies without cloning the key directory.
    pub fn body_count(&self) -> u64 {
        u64::try_from(self.bodies.len()).unwrap_or(u64::MAX)
    }

    /// Release one immutable Atomic writer image after its durable causal
    /// closure has been published elsewhere.
    ///
    /// Fabric deliberately does not retain a parallel semantic directory:
    /// callers must own the durable Body presence/binding/material record and
    /// re-import the exact verified snapshot before an operation whose
    /// semantics require the prior value. Collaborative Bodies are never
    /// released through this seam because their live causal writer state is
    /// richer than a whole-value Atomic replacement.
    pub fn release_atomic_image(&mut self, key: &Key) -> bool {
        if matches!(self.bodies.get(key), Some(BodyState::Atomic(_))) {
            self.bodies.remove(key);
            self.hot.retain(|candidate| candidate != key);
            true
        } else {
            false
        }
    }

    fn loro_err(e: impl std::fmt::Display) -> Failure {
        tracing::warn!(%e, "fabric operation was invalid");
        Failure::Invalid(commit::Invalid::Import)
    }

    fn cool_body(&mut self, key: &Key) -> Result<(), Failure> {
        let Some(state) = self.bodies.get_mut(key) else {
            return Ok(());
        };
        if let BodyState::Collab(doc) = state {
            *state = BodyState::FrozenCollab(FrozenCollab::from_doc(doc)?);
        }
        self.hot.retain(|candidate| candidate != key);
        Ok(())
    }

    fn reserve_hot(&mut self, key: &Key) -> Result<(), Failure> {
        self.hot.retain(|candidate| candidate != key);
        while self.hot.len() >= MAX_HOT_COLLABORATIVE_BODIES {
            let Some(cold) = self.hot.pop_front() else {
                break;
            };
            if &cold != key {
                self.cool_body(&cold)?;
                if self.external_collaborative_images {
                    self.external_evicted.push_back(cold);
                }
            }
        }
        Ok(())
    }

    fn drain_external_evictions(&mut self) {
        if !self.external_collaborative_images {
            self.external_evicted.clear();
            return;
        }
        while let Some(key) = self.external_evicted.pop_front() {
            if self.hot.iter().any(|hot| hot == &key) {
                continue;
            }
            if matches!(self.bodies.get(&key), Some(BodyState::FrozenCollab(_))) {
                self.bodies.remove(&key);
            }
        }
    }

    fn make_hot(&mut self, key: &Key) -> Result<(), Failure> {
        let frozen = match self.bodies.get(key) {
            Some(BodyState::FrozenCollab(frozen)) => Some(frozen.clone()),
            Some(BodyState::Collab(_)) => None,
            Some(BodyState::Atomic(_)) | None => return Ok(()),
        };
        self.reserve_hot(key)?;
        if let Some(frozen) = frozen {
            let doc = frozen.doc(Some(self.writer))?;
            self.bodies.insert(key.clone(), BodyState::Collab(doc));
        }
        self.hot.push_back(key.clone());
        Ok(())
    }

    /// The collaborative doc for a Body, creating it when `create`. An atomic
    /// value at the key is a [`Failure::TypeConflict`].
    fn collab_doc(&mut self, key: &Key, create: bool) -> Result<Option<&loro::LoroDoc>, Failure> {
        use std::collections::btree_map::Entry;
        self.make_hot(key)?;
        let creating = !self.bodies.contains_key(key) && create;
        if creating {
            self.reserve_hot(key)?;
            self.hot.push_back(key.clone());
        }
        match self.bodies.entry(key.clone()) {
            Entry::Occupied(e) => match e.into_mut() {
                BodyState::Collab(doc) => Ok(Some(doc)),
                BodyState::Atomic(_) => Err(Failure::TypeConflict),
                BodyState::FrozenCollab(_) => Err(Failure::OutcomeUnknown),
            },
            Entry::Vacant(v) if create => {
                match v.insert(BodyState::Collab(new_body_doc(Some(self.writer)))) {
                    BodyState::Collab(doc) => Ok(Some(doc)),
                    BodyState::Atomic(_) => Err(Failure::TypeConflict),
                    BodyState::FrozenCollab(_) => Err(Failure::OutcomeUnknown),
                }
            }
            Entry::Vacant(_) => Ok(None),
        }
    }

    /// Enforce "a path is bound to exactly one collaborative type": no other
    /// type tag may already hold state at this path. Containers live at doc
    /// ROOTS (name-identified — see the struct docs); a register is a key in
    /// the `body` root map.
    ///
    /// The sibling tags are checked against the roots the document ACTUALLY
    /// has, from `get_value`, before any accessor is called. That ordering is
    /// the whole point. `doc.get_movable_list(name)` and its siblings CREATE
    /// the root they name — root containers need no operation to exist — so
    /// asking "is this path already a list?" the direct way made it one.
    ///
    /// Every typed write probed five siblings, so every path permanently
    /// materialised four empty phantom roots, and they replicated: a Body with
    /// a single counter path `votes` projected as also having a map, a list, a
    /// text and a set named `votes`, on every peer that imported it. Measured,
    /// that was a 5x inflated projection and ~150% snapshot overhead at 512
    /// paths (415 -> 295 bytes at one path, 25,206 -> 9,988 at 512).
    ///
    /// `has_container` is not the check to use here: a `ContainerID::Root` is
    /// implicit, so it answers true for every name that could exist. The root
    /// listing is what distinguishes a container that was written from one that
    /// was merely named — and it keeps distinguishing correctly for a container
    /// that was written and then emptied, which stays in the listing.
    fn check_path_type(doc: &loro::LoroDoc, tag: &str, path: &str) -> Result<(), Failure> {
        let body = doc.get_map(BODY_MAP);
        // Shallow: the root names and their values, without walking into the
        // containers. A register lookup still needs `body`, which is a root this
        // Body always has.
        let roots = doc.get_value();
        let existing = match &roots {
            loro::LoroValue::Map(map) => Some(map),
            _ => None,
        };
        for other in TYPE_TAGS {
            if other == tag {
                continue;
            }
            let name = typed_key(other, path);
            if other == "reg" {
                if body.get(&name).is_some() {
                    return Err(Failure::TypeConflict);
                }
                continue;
            }
            // Not a root this document has: nothing is bound here, and asking
            // any harder would bind it.
            if !existing.is_some_and(|map| map.contains_key(name.as_str())) {
                continue;
            }
            let bound = match other {
                "list" | "log" => !doc.get_movable_list(name.as_str()).is_empty(),
                "text" => !doc.get_text(name.as_str()).is_empty(),
                "tree" => !doc.get_tree(name.as_str()).is_empty(),
                _ => !doc.get_map(name.as_str()).is_empty(),
            };
            if bound {
                return Err(Failure::TypeConflict);
            }
        }
        Ok(())
    }

    /// The Body's doc for a typed-path write, with the path-type binding
    /// enforced.
    fn doc_for(&mut self, key: &Key, tag: &str, path: &str) -> Result<&loro::LoroDoc, Failure> {
        let doc = self.collab_doc(key, true)?.ok_or(Failure::TypeConflict)?;
        Self::check_path_type(doc, tag, path)?;
        Ok(doc)
    }

    /// The `(index, decoded element blob)` pairs of a list, skipping malformed
    /// entries (which canonical writes never produce).
    fn list_entries(l: &loro::LoroMovableList) -> Vec<(usize, String, Vec<u8>)> {
        let mut out = Vec::new();
        for i in 0..l.len() {
            let Some(v) = l.get(i) else { continue };
            let Some(bytes) = v
                .into_value()
                .ok()
                .and_then(|val| val.into_binary().ok())
                .map(|b| b.to_vec())
            else {
                continue;
            };
            if bytes.len() < ELEMENT_ID_LEN {
                continue;
            }
            let Some((identity, value)) = bytes.split_at_checked(ELEMENT_ID_LEN) else {
                continue;
            };
            let id = data_encoding::HEXLOWER.encode(identity);
            out.push((i, id, value.to_vec()));
        }
        out
    }

    /// The tree at a path, with the ordering guarantee this crate depends on
    /// made explicit rather than inherited.
    ///
    /// Loro 1.13.6 generates fractional indices by default, and sibling order
    /// is exactly that generation: a tree with it disabled gives every node the
    /// same default position, so siblings order by an internal tiebreak and
    /// `after` placement stops meaning anything. Asserting it on every write
    /// costs a field assignment (the call is idempotent, and jitter 0 never
    /// touches the generator's rng) and removes a silent dependency on a
    /// library default.
    fn tree_at(doc: &loro::LoroDoc, path: &str) -> loro::LoroTree {
        let tree = doc.get_tree(typed_key("tree", path).as_str());
        tree.enable_fractional_index(0);
        tree
    }

    /// Resolve an opaque node id against a tree: it must parse, exist, and not
    /// be deleted. A deleted node is `UnknownElement` rather than a silent
    /// success — hanging a child off a removed parent would put the child in
    /// the deleted subtree, where no reader would ever see it again.
    fn tree_node(tree: &loro::LoroTree, node: &str) -> Result<loro::TreeID, Failure> {
        let unknown = || Failure::Invalid(commit::Invalid::UnknownElement);
        let id = loro::TreeID::try_from(node).map_err(|_| unknown())?;
        if !tree.contains(id) || tree.is_node_deleted(&id).unwrap_or(true) {
            return Err(unknown());
        }
        Ok(id)
    }

    /// The node carrying an application anchor, created at a root of the forest
    /// if none does yet.
    ///
    /// Resolution is the lowest node id among the nodes carrying the anchor, so
    /// two replicas that created one concurrently agree which one the anchor
    /// means without either having to observe the other first.
    fn tree_anchored(tree: &loro::LoroTree, anchor: &str) -> Result<loro::TreeID, Failure> {
        let mut found: Option<loro::TreeID> = None;
        for node in tree.get_nodes(false) {
            let meta = tree.get_meta(node.id).map_err(Self::tree_err)?;
            let carries = meta
                .get(NODE_ANCHOR_KEY)
                .and_then(|v| v.into_value().ok())
                .and_then(|v| v.into_binary().ok())
                .is_some_and(|bytes| bytes.as_slice() == anchor.as_bytes());
            if !carries {
                continue;
            }
            let lower = found
                .is_none_or(|best| (node.id.peer, node.id.counter) < (best.peer, best.counter));
            if lower {
                found = Some(node.id);
            }
        }
        if let Some(node) = found {
            return Ok(node);
        }
        let node = tree
            .create(loro::TreeParentId::Root)
            .map_err(Self::tree_err)?;
        tree.get_meta(node)
            .map_err(Self::tree_err)?
            .insert(NODE_ANCHOR_KEY, anchor.as_bytes())
            .map_err(Self::loro_err)?;
        Ok(node)
    }

    /// The parent a node hangs under, as this crate names parents: `None` at a
    /// root of the forest.
    fn tree_parent_of(tree: &loro::LoroTree, node: loro::TreeID) -> Option<loro::TreeID> {
        match tree.parent(node) {
            Some(loro::TreeParentId::Node(parent)) => Some(parent),
            _ => None,
        }
    }

    /// Resolve a placement into the parent it names and the sibling it is to
    /// follow, refusing a sibling that is not a child of that parent.
    ///
    /// Shared by insert and move because the placement rules must not differ
    /// between them, and cross-checked because Loro's `mov_after` takes the
    /// sibling's parent as the destination — an unchecked `after` would
    /// silently overrule the `parent` the same operation named.
    fn tree_target(
        tree: &loro::LoroTree,
        parent: Option<&String>,
        after: Option<&String>,
    ) -> Result<(loro::TreeParentId, Option<loro::TreeID>), Failure> {
        let parent_id = parent
            .map(|p| Self::tree_node(tree, p))
            .transpose()?
            .map_or(loro::TreeParentId::Root, loro::TreeParentId::Node);
        let sibling = after.map(|a| Self::tree_node(tree, a)).transpose()?;
        if let Some(sibling) = sibling {
            let sibling_parent = Self::tree_parent_of(tree, sibling)
                .map_or(loro::TreeParentId::Root, loro::TreeParentId::Node);
            if sibling_parent != parent_id {
                return Err(Failure::Invalid(commit::Invalid::TreePlacement));
            }
        }
        Ok((parent_id, sibling))
    }

    /// Loro tree errors, keeping the cycle refusal distinguishable from the
    /// rest. A caller can act on a cycle — it asked for an impossible
    /// hierarchy — and cannot act on anything else here.
    fn tree_err(e: loro::LoroError) -> Failure {
        if matches!(
            e,
            loro::LoroError::TreeError(loro::LoroTreeError::CyclicMoveError)
        ) {
            return Failure::Invalid(commit::Invalid::TreeCycle);
        }
        Self::loro_err(e)
    }

    /// The causal token digesting the touched Bodies' post-commit positions.
    fn causal_for(
        &self,
        touched: &std::collections::BTreeSet<Key>,
    ) -> Result<CausalToken, Failure> {
        let mut h = blake3::Hasher::new();
        h.update(CAUSAL_DOMAIN);
        for key in touched {
            let key_len = u64::try_from(key.as_bytes().len()).unwrap_or(u64::MAX);
            h.update(&key_len.to_le_bytes());
            h.update(key.as_bytes());
            match self.bodies.get(key) {
                Some(state) => {
                    let digest = state.digest()?;
                    h.update(&[1]);
                    let digest_len = u64::try_from(digest.len()).unwrap_or(u64::MAX);
                    h.update(&digest_len.to_le_bytes());
                    h.update(&digest);
                }
                None => {
                    h.update(&[0]);
                }
            }
        }
        Ok(CausalToken::from_bytes(h.finalize().as_bytes().to_vec()))
    }

    /// Apply one operation. Errors leave partially-applied state in the touched
    /// Body; [`fabric::commit`] rolls the whole batch back from its backups.
    fn apply(&mut self, op: &Op) -> Result<(), Failure> {
        match op {
            Op::PutCanonical { key, value } => {
                if matches!(
                    self.bodies.get(key),
                    Some(BodyState::Collab(_) | BodyState::FrozenCollab(_))
                ) {
                    // A collaborative Body cannot be silently flattened.
                    return Err(Failure::TypeConflict);
                }
                self.bodies
                    .insert(key.clone(), BodyState::Atomic(value.clone().into()));
                Ok(())
            }
            Op::Remove { key } => {
                self.bodies.remove(key);
                Ok(())
            }
            Op::CreateBody { key } => {
                self.collab_doc(key, true)?;
                Ok(())
            }
            Op::RegisterSet { key, path, value } => {
                let doc = self.doc_for(key, "reg", path)?;
                let body = doc.get_map(BODY_MAP);
                body.insert(&typed_key("reg", path), value.as_slice())
                    .map_err(Self::loro_err)
            }
            Op::RegisterClear { key, path } => {
                let doc = self.doc_for(key, "reg", path)?;
                let body = doc.get_map(BODY_MAP);
                let k = typed_key("reg", path);
                if body.get(&k).is_some() {
                    body.delete(&k).map_err(Self::loro_err)?;
                }
                Ok(())
            }
            Op::MapSet {
                key,
                path,
                entry,
                value,
            } => {
                let doc = self.doc_for(key, "map", path)?;
                let m = doc.get_map(typed_key("map", path).as_str());
                m.insert(entry, value.as_slice()).map_err(Self::loro_err)
            }
            Op::MapRemove { key, path, entry } => {
                let doc = self.doc_for(key, "map", path)?;
                let m = doc.get_map(typed_key("map", path).as_str());
                if m.get(entry).is_some() {
                    m.delete(entry).map_err(Self::loro_err)?;
                }
                Ok(())
            }
            Op::ListInsert {
                key,
                path,
                index,
                value,
            } => {
                let doc = self.doc_for(key, "list", path)?;
                let l = doc.get_movable_list(typed_key("list", path).as_str());
                let index = usize::try_from(*index)
                    .map_err(|_| Failure::Invalid(commit::Invalid::ListIndex))?;
                if index > l.len() {
                    return Err(Failure::Invalid(commit::Invalid::ListIndex));
                }
                // Engine mints the stable element id and embeds it in the value,
                // so identity survives synchronization.
                let id: [u8; ELEMENT_ID_LEN] = mint_bytes();
                let capacity = ELEMENT_ID_LEN.saturating_add(value.len());
                let mut blob = Vec::with_capacity(capacity);
                blob.extend_from_slice(&id);
                blob.extend_from_slice(value);
                l.insert(index, blob.as_slice()).map_err(Self::loro_err)
            }
            Op::ListRemove { key, path, element } => {
                let doc = self.doc_for(key, "list", path)?;
                let l = doc.get_movable_list(typed_key("list", path).as_str());
                let Some((i, _, _)) = Self::list_entries(&l)
                    .into_iter()
                    .find(|(_, id, _)| id == element)
                else {
                    return Err(Failure::Invalid(commit::Invalid::UnknownElement));
                };
                l.delete(i, 1).map_err(Self::loro_err)
            }
            Op::ListMove {
                key,
                path,
                element,
                index,
            } => {
                let doc = self.doc_for(key, "list", path)?;
                let l = doc.get_movable_list(typed_key("list", path).as_str());
                let Some((from, _, _)) = Self::list_entries(&l)
                    .into_iter()
                    .find(|(_, id, _)| id == element)
                else {
                    return Err(Failure::Invalid(commit::Invalid::UnknownElement));
                };
                let to = usize::try_from(*index)
                    .map_err(|_| Failure::Invalid(commit::Invalid::ListIndex))?;
                if to >= l.len() {
                    return Err(Failure::Invalid(commit::Invalid::ListIndex));
                }
                l.mov(from, to).map_err(Self::loro_err)
            }
            Op::TextSplice {
                key,
                path,
                index,
                delete,
                insert,
            } => {
                let doc = self.doc_for(key, "text", path)?;
                let t = doc.get_text(typed_key("text", path).as_str());
                let len = t.to_string().chars().count();
                let index = usize::try_from(*index)
                    .map_err(|_| Failure::Invalid(commit::Invalid::TextRange))?;
                let delete = usize::try_from(*delete)
                    .map_err(|_| Failure::Invalid(commit::Invalid::TextRange))?;
                if index.checked_add(delete).is_none_or(|end| end > len) {
                    return Err(Failure::Invalid(commit::Invalid::TextRange));
                }
                if delete > 0 {
                    t.delete(index, delete).map_err(Self::loro_err)?;
                }
                if !insert.is_empty() {
                    t.insert(index, insert).map_err(Self::loro_err)?;
                }
                Ok(())
            }
            Op::SetAdd { key, path, value } => {
                let doc = self.doc_for(key, "set", path)?;
                let m = doc.get_map(typed_key("set", path).as_str());
                // Observed-remove set: every add mints a fresh tag, so a remove
                // only deletes the adds it has seen — a concurrent add survives.
                let tag: [u8; 8] = mint_bytes();
                let member = format!(
                    "{}:{}",
                    set_member_prefix(value),
                    data_encoding::HEXLOWER.encode(&tag)
                );
                m.insert(&member, value.as_slice()).map_err(Self::loro_err)
            }
            Op::SetRemove { key, path, value } => {
                let doc = self.doc_for(key, "set", path)?;
                let m = doc.get_map(typed_key("set", path).as_str());
                let prefix = format!("{}:", set_member_prefix(value));
                for k in crate::loro_ext::map_keys(&m) {
                    if k.starts_with(&prefix) {
                        m.delete(&k).map_err(Self::loro_err)?;
                    }
                }
                Ok(())
            }
            Op::CounterAdd { key, path, delta } => {
                let doc = self.doc_for(key, "cnt", path)?;
                let peer = doc.peer_id();
                let m = doc.get_map(typed_key("cnt", path).as_str());
                // PN-counter: each doc session sums into its own peer key;
                // concurrent increments land in disjoint keys and always sum.
                let me = peer.to_string();
                let current = crate::loro_ext::get_i64(&m, &me).unwrap_or(0);
                let next = current
                    .checked_add(*delta)
                    .ok_or(Failure::Invalid(commit::Invalid::CounterOverflow))?;
                m.insert(&me, next).map_err(Self::loro_err)
            }
            Op::TreeInsert {
                key,
                path,
                parent,
                after,
                value,
            } => {
                let doc = self.doc_for(key, "tree", path)?;
                let tree = Self::tree_at(doc, path);
                // Resolved and cross-checked BEFORE the node exists. A refusal
                // rolls the batch back either way, but a create that has
                // already happened is history a compensating operation has to
                // undo — and the common case, an append with no `after`, is
                // then one Loro operation rather than a create plus a move.
                let (parent_id, sibling) =
                    Self::tree_target(&tree, parent.as_ref(), after.as_ref())?;
                let node = tree.create(parent_id).map_err(Self::tree_err)?;
                if let Some(sibling) = sibling {
                    tree.mov_after(node, sibling).map_err(Self::tree_err)?;
                }
                tree.get_meta(node)
                    .map_err(Self::tree_err)?
                    .insert(NODE_VALUE_KEY, value.as_slice())
                    .map_err(Self::loro_err)
            }
            Op::TreeMove {
                key,
                path,
                node,
                parent,
                after,
            } => {
                let doc = self.doc_for(key, "tree", path)?;
                let tree = Self::tree_at(doc, path);
                let node = Self::tree_node(&tree, node)?;
                let (parent_id, sibling) =
                    Self::tree_target(&tree, parent.as_ref(), after.as_ref())?;
                match sibling {
                    // A node cannot follow itself, and Loro would answer that
                    // request with a move to where it already is — a silent
                    // success for an operation that named nothing coherent.
                    Some(sibling) if sibling == node => {
                        Err(Failure::Invalid(commit::Invalid::TreePlacement))
                    }
                    Some(sibling) => tree.mov_after(node, sibling).map_err(Self::tree_err),
                    None => tree.mov(node, parent_id).map_err(Self::tree_err),
                }
            }
            Op::TreeRemove { key, path, node } => {
                let doc = self.doc_for(key, "tree", path)?;
                let tree = Self::tree_at(doc, path);
                let node = Self::tree_node(&tree, node)?;
                tree.delete(node).map_err(Self::tree_err)
            }
            Op::TreeSet {
                key,
                path,
                node,
                entry,
                value,
            } => {
                if entry == NODE_VALUE_KEY {
                    return Err(Failure::Invalid(commit::Invalid::TreeReservedEntry));
                }
                let doc = self.doc_for(key, "tree", path)?;
                let tree = Self::tree_at(doc, path);
                let node = Self::tree_node(&tree, node)?;
                tree.get_meta(node)
                    .map_err(Self::tree_err)?
                    .insert(entry, value.as_slice())
                    .map_err(Self::loro_err)
            }
            Op::TreeUnset {
                key,
                path,
                node,
                entry,
            } => {
                if entry == NODE_VALUE_KEY {
                    return Err(Failure::Invalid(commit::Invalid::TreeReservedEntry));
                }
                let doc = self.doc_for(key, "tree", path)?;
                let tree = Self::tree_at(doc, path);
                let node = Self::tree_node(&tree, node)?;
                let meta = tree.get_meta(node).map_err(Self::tree_err)?;
                if meta.get(entry).is_some() {
                    meta.delete(entry).map_err(Self::loro_err)?;
                }
                Ok(())
            }
            Op::TreeAnchor {
                key,
                path,
                anchor,
                parent,
            } => {
                // A node cannot be filed under itself, and the anchor form is
                // where that is easy to ask for by accident — the two fields
                // are both application keys, so a caller looping over records
                // can hand the same one to both.
                if parent.as_deref() == Some(anchor.as_str()) {
                    return Err(Failure::Invalid(commit::Invalid::TreePlacement));
                }
                let doc = self.doc_for(key, "tree", path)?;
                let tree = Self::tree_at(doc, path);
                let node = Self::tree_anchored(&tree, anchor)?;
                let parent_id = match parent {
                    None => loro::TreeParentId::Root,
                    Some(parent) => loro::TreeParentId::Node(Self::tree_anchored(&tree, parent)?),
                };
                // Already where it was asked to be. Skipped rather than
                // re-recorded: this operation is idempotent by design, and a
                // move that changes nothing is still history every replica
                // stores and syncs.
                if tree.parent(node) == Some(parent_id) {
                    return Ok(());
                }
                tree.mov(node, parent_id).map_err(Self::tree_err)
            }
            Op::LogAppend {
                key,
                path,
                value,
                retain,
            } => {
                let doc = self.doc_for(key, "log", path)?;
                // Entries reuse the list encoding — `element_id[16] || value` in
                // a movable list — so a log entry has the same stable identity a
                // list element does. That identity is what a reader's cursor
                // holds: a position cannot survive trimming, because trimming
                // renumbers every entry behind it.
                let entries = doc.get_movable_list(typed_key("log", path).as_str());
                let id: [u8; ELEMENT_ID_LEN] = mint_bytes();
                let capacity = ELEMENT_ID_LEN.saturating_add(value.len());
                let mut blob = Vec::with_capacity(capacity);
                blob.extend_from_slice(&id);
                blob.extend_from_slice(value);
                entries
                    .insert(entries.len(), blob.as_slice())
                    .map_err(Self::loro_err)?;

                let peer = doc.peer_id();
                let counts = doc.get_map(typed_key(LOG_COUNT_TAG, path).as_str());
                let me = peer.to_string();
                let current = crate::loro_ext::get_i64(&counts, &me).unwrap_or(0);
                let next = current
                    .checked_add(1)
                    .ok_or(Failure::Invalid(commit::Invalid::CounterOverflow))?;
                counts.insert(&me, next).map_err(Self::loro_err)?;

                // `retain` of zero would empty the log on every append, which
                // is a log that cannot be read — refused rather than obeyed.
                let retain = usize::try_from(*retain)
                    .map_err(|_| Failure::Invalid(commit::Invalid::Bounds))?;
                if retain == 0 {
                    return Err(Failure::Invalid(commit::Invalid::Bounds));
                }
                let over = entries.len().saturating_sub(retain);
                if over > 0 {
                    entries.delete(0, over).map_err(Self::loro_err)?;
                }
                Ok(())
            }
        }
    }

    /// The key an operation touches.
    fn op_key(op: &Op) -> &Key {
        match op {
            Op::PutCanonical { key, .. }
            | Op::Remove { key }
            | Op::CreateBody { key }
            | Op::RegisterSet { key, .. }
            | Op::RegisterClear { key, .. }
            | Op::MapSet { key, .. }
            | Op::MapRemove { key, .. }
            | Op::ListInsert { key, .. }
            | Op::ListRemove { key, .. }
            | Op::ListMove { key, .. }
            | Op::TextSplice { key, .. }
            | Op::SetAdd { key, .. }
            | Op::SetRemove { key, .. }
            | Op::CounterAdd { key, .. }
            | Op::TreeInsert { key, .. }
            | Op::TreeMove { key, .. }
            | Op::TreeRemove { key, .. }
            | Op::TreeSet { key, .. }
            | Op::TreeUnset { key, .. }
            | Op::TreeAnchor { key, .. }
            | Op::LogAppend { key, .. } => key,
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// Apply a transaction into the live candidate image without publishing
    /// it. Reads and exports observe the candidate until it is finalized or
    /// rolled back.
    pub fn prepare(&mut self, request: Transaction) -> Result<Prepared, Failure> {
        // Batch atomicity, bounded. This used to back every touched Body up by
        // full export before applying — a complete-history snapshot per
        // ordinary edit, paid inside Engine before anything was sealed, and no
        // amount of delta-shaped sealing downstream removed it.
        //
        // What replaces it is a *position*: one head set per touched Body,
        // which is one or two operation ids. On failure the engine reverts to
        // that position by applying the inverse of what was done, at a cost
        // proportional to the failed batch rather than to the Body's life.
        //
        // Reverting appends compensating operations rather than erasing the
        // failed ones, and that is the honest thing for a CRDT to do — you
        // cannot unsay an operation another replica may already hold. A failed
        // batch therefore costs a little history and no correctness, and a
        // checkpoint reclaims it.
        let touched: std::collections::BTreeSet<Key> = request
            .ops
            .iter()
            .map(|op| Self::op_key(op).clone())
            .collect();
        //
        // A position alone is not enough, though. `Remove` destroys the doc the
        // position indexes, and a `CreateBody` after it installs a *different*
        // doc whose history has never seen those operation ids — so the
        // snapshot holds the Body's whole prior state as well. That is cheap:
        // a `LoroDoc` clone is a reference clone sharing one underlying
        // document, so putting the clone back restores the original Body
        // rather than a copy of it.
        let prior: BTreeMap<Key, Option<(BodyState, Option<loro::Frontiers>)>> = touched
            .iter()
            .map(|key| {
                let snapshot = self.bodies.get(key).map(|state| match state {
                    // An atomic Body's whole value *is* its state, and it is
                    // already bounded by the Body envelope.
                    BodyState::Atomic(bytes) => (BodyState::Atomic(bytes.clone()), None),
                    BodyState::Collab(doc) => {
                        (BodyState::Collab(doc.clone()), Some(doc.oplog_frontiers()))
                    }
                    BodyState::FrozenCollab(frozen) => {
                        (BodyState::FrozenCollab(frozen.clone()), None)
                    }
                });
                (key.clone(), snapshot)
            })
            .collect();

        let mut failed = None;
        for op in &request.ops {
            if let Err(e) = self.apply(op) {
                failed = Some(e);
                break;
            }
        }
        if let Some(e) = failed {
            // Total, not best-effort. One Body that will not revert must not
            // abandon the rest of the batch half-applied, so every key is put
            // back before anything is reported.
            self.restore(prior)?;
            return Err(e);
        }
        // Seal each touched collaborative doc's staged change as one labelled
        // Loro commit.
        for key in &touched {
            if let Some(BodyState::Collab(doc)) = self.bodies.get(key) {
                doc.set_next_commit_message(&request.request);
                doc.commit();
            }
        }
        let applied = u32::try_from(request.ops.len())
            .map_err(|_| Failure::Invalid(commit::Invalid::Bounds))?;
        Ok(Prepared {
            receipt: Receipt::new(self.causal_for(&touched)?, applied),
            prior,
        })
    }

    /// Accept one prepared candidate. Durability is owned by Replica, so the
    /// Engine has no additional write to perform here. Finalized collaborative
    /// Bodies remain in the bounded mutation-hot set: the next edit can reuse
    /// its `LoroDoc`, while [`Engine::reserve_hot`] freezes the least-recently
    /// used Body when the cap is reached.
    pub fn finalize(&mut self, prepared: Prepared) -> Receipt {
        let receipt = prepared.receipt;
        self.drain_external_evictions();
        receipt
    }

    /// Restore the exact semantic state that preceded a preparation.
    pub fn rollback(&mut self, prepared: Prepared) -> Result<(), Failure> {
        self.restore(prepared.prior)
    }

    /// Preserve the original one-call surface for callers that do not need a
    /// validation phase.
    pub fn commit(&mut self, request: Transaction) -> Result<Receipt, Failure> {
        let prepared = self.prepare(request)?;
        Ok(self.finalize(prepared))
    }

    fn restore(
        &mut self,
        prior: BTreeMap<Key, Option<(BodyState, Option<loro::Frontiers>)>>,
    ) -> Result<(), Failure> {
        let mut unrestored = 0usize;
        for (key, snapshot) in prior {
            self.hot.retain(|candidate| candidate != &key);
            let Some((state, frontiers)) = snapshot else {
                // The Body did not exist before this batch, so undoing the
                // batch means it does not exist now either.
                self.bodies.remove(&key);
                continue;
            };
            self.bodies.insert(key.clone(), state);
            if let (Some(frontiers), Some(BodyState::Collab(doc))) =
                (frontiers, self.bodies.get(&key))
            {
                doc.commit();
                if doc.revert_to(&frontiers).is_err() {
                    unrestored = unrestored.saturating_add(1);
                }
            }
            if matches!(self.bodies.get(&key), Some(BodyState::Collab(_))) {
                self.hot.push_back(key);
            }
        }
        if unrestored > 0 {
            tracing::error!(unrestored, "fabric rollback did not restore all bodies");
            return Err(Failure::OutcomeUnknown);
        }
        self.drain_external_evictions();
        Ok(())
    }

    pub fn read(&self, key: &Key) -> Option<Vec<u8>> {
        match self.bodies.get(key)? {
            BodyState::Atomic(bytes) => Some(bytes.to_vec()),
            BodyState::Collab(_) | BodyState::FrozenCollab(_) => None,
        }
    }

    pub fn read_collaborative(&self, key: &Key) -> Result<CollaborativeView, ProjectionFailure> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => project_collaborative_doc(doc),
            Some(BodyState::FrozenCollab(frozen)) => frozen
                .doc(None)
                .map_err(|_| ProjectionFailure::Malformed)
                .and_then(|doc| project_collaborative_doc(&doc)),
            _ => Err(ProjectionFailure::NotCollaborative),
        }
    }

    pub fn version(&self, key: &Key) -> Result<crate::causal::Version, crate::causal::Invalid> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => Ok(crate::causal::Version::from_frontiers(
                &doc.oplog_frontiers(),
            )),
            Some(BodyState::FrozenCollab(frozen)) => {
                frozen.version().map_err(|_| crate::causal::Invalid::Engine)
            }
            // An atomic Body has one writer at a time and no operation history,
            // so its position is the empty one. It still answers, because a
            // caller should not have to know which model a key holds to ask.
            Some(BodyState::Atomic(_)) => Ok(crate::causal::Version::empty()),
            None => Err(crate::causal::Invalid::NotCollaborative),
        }
    }

    pub fn export_delta(
        &self,
        key: &Key,
        from: &crate::causal::Version,
    ) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
        from.validate()?;
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => export_delta_from_doc(doc, from),
            Some(BodyState::FrozenCollab(frozen)) => frozen
                .doc(None)
                .map_err(|_| crate::causal::Invalid::Engine)
                .and_then(|doc| export_delta_from_doc(&doc, from)),
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::Artifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.to_vec(),
            }),
            None => Err(crate::causal::Invalid::NotCollaborative),
        }
    }

    pub fn export_checkpoint(
        &self,
        key: &Key,
        retention_frontier: &crate::causal::Version,
    ) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
        retention_frontier.validate()?;
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => export_checkpoint_from_doc(doc, retention_frontier),
            Some(BodyState::FrozenCollab(frozen)) => frozen
                .doc(None)
                .map_err(|_| crate::causal::Invalid::Engine)
                .and_then(|doc| export_checkpoint_from_doc(&doc, retention_frontier)),
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::Artifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.to_vec(),
            }),
            None => Err(crate::causal::Invalid::NotCollaborative),
        }
    }

    pub fn export_history(
        &self,
        key: &Key,
    ) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => export_history_from_doc(doc),
            Some(BodyState::FrozenCollab(frozen)) => frozen
                .doc(None)
                .map_err(|_| crate::causal::Invalid::Engine)
                .and_then(|doc| export_history_from_doc(&doc)),
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::Artifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.to_vec(),
            }),
            None => Err(crate::causal::Invalid::NotCollaborative),
        }
    }

    pub fn import_artifact(
        &mut self,
        key: &Key,
        artifact: &crate::causal::Artifact,
    ) -> Result<crate::causal::ImportStatus, crate::causal::Invalid> {
        use crate::causal::{Artifact, ImportStatus, Invalid, Version};

        if let Artifact::Replace { bytes, .. } = artifact {
            // A model mismatch is a conflict, not an overwrite. `import_body`
            // refuses the same pair, and the reverse direction — a delta onto
            // an atomic Body — is refused below; flattening a collaborative
            // Body into a value here would discard its whole history because a
            // peer sent the wrong artifact kind.
            if matches!(
                self.bodies.get(key),
                Some(BodyState::Collab(_) | BodyState::FrozenCollab(_))
            ) {
                return Err(Invalid::NotCollaborative);
            }
            let changed = !matches!(
                self.bodies.get(key),
                Some(BodyState::Atomic(current)) if current.as_ref() == bytes.as_slice()
            );
            if changed {
                self.bodies
                    .insert(key.clone(), BodyState::Atomic(bytes.clone().into()));
            }
            return Ok(ImportStatus {
                applied: changed,
                pending: false,
            });
        }

        let bytes = match artifact {
            Artifact::Delta { bytes, .. }
            | Artifact::Checkpoint { bytes, .. }
            | Artifact::Archive { bytes, .. } => bytes,
            Artifact::Replace { .. } => return Err(Invalid::Engine),
        };

        let doc = self
            .collab_doc(key, true)
            .map_err(|_| Invalid::Engine)?
            .ok_or(Invalid::NotCollaborative)?;
        let before = doc.oplog_frontiers();
        let status = doc.import(bytes).map_err(|e| {
            // A compacted document refuses work whose dependencies precede its
            // shallow root. That refusal is the whole of §5.2 outcome 2, and it
            // is named rather than reported as a generic engine error so a
            // writer can act on it: rebuild from the archive, or re-bootstrap.
            let message = e.to_string();
            if message.contains("shallow") || message.contains("Shallow") {
                Invalid::BeforeRetentionFrontier {
                    frontier: Version::from_frontiers(&doc.shallow_since_frontiers()),
                }
            } else {
                tracing::warn!(%message, "fabric import failed");
                Invalid::Engine
            }
        })?;
        let after = doc.oplog_frontiers();
        Ok(ImportStatus {
            applied: before != after,
            pending: status.pending.is_some(),
        })
    }

    pub fn relation(
        &self,
        key: &Key,
        a: &crate::causal::Version,
        b: &crate::causal::Version,
    ) -> crate::causal::CausalRelation {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => crate::causal::relation(doc, a, b),
            Some(BodyState::FrozenCollab(frozen)) => frozen
                .doc(None)
                .map(|doc| crate::causal::relation(&doc, a, b))
                .unwrap_or(crate::causal::CausalRelation::Undetermined),
            _ => crate::causal::CausalRelation::Undetermined,
        }
    }

    pub fn anchor(
        &self,
        key: &Key,
        path: &str,
        position: u64,
    ) -> Result<crate::causal::Anchor, crate::causal::Invalid> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => anchor_in_doc(doc, key, path, position),
            Some(BodyState::FrozenCollab(frozen)) => frozen
                .doc(None)
                .map_err(|_| crate::causal::Invalid::Engine)
                .and_then(|doc| anchor_in_doc(&doc, key, path, position)),
            _ => Err(crate::causal::Invalid::NotCollaborative),
        }
    }

    pub fn resolve(
        &self,
        key: &Key,
        anchor: &crate::causal::Anchor,
    ) -> crate::causal::AnchorResolution {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => resolve_in_doc(doc, key, anchor),
            Some(BodyState::FrozenCollab(frozen)) => frozen
                .doc(None)
                .map(|doc| resolve_in_doc(&doc, key, anchor))
                .unwrap_or(crate::causal::AnchorResolution::Drifted),
            _ => crate::causal::AnchorResolution::Drifted,
        }
    }

    pub fn export_body(&self, key: &Key) -> Option<BodyExport> {
        self.bodies.get(key).and_then(|s| s.export().ok())
    }

    /// Freeze one immutable read image. Cold collaborative Bodies share their
    /// canonical export and compact version with the Engine; only one of the
    /// bounded mutation-hot documents must serialize here.
    pub fn body_snapshot(&self, key: &Key) -> Result<Option<BodySnapshot>, Failure> {
        match self.bodies.get(key) {
            None => Ok(None),
            Some(BodyState::Atomic(bytes)) => Ok(Some(BodySnapshot::from_atomic(bytes.clone()))),
            Some(BodyState::FrozenCollab(frozen)) => Ok(Some(BodySnapshot::from_frozen(frozen))),
            Some(BodyState::Collab(doc)) => {
                let frozen = FrozenCollab::from_doc(doc)?;
                Ok(Some(BodySnapshot::from_frozen(&frozen)))
            }
        }
    }

    /// Adopt a Body image whose material has already been interpreted and
    /// verified by another Engine in the same recovery pipeline.
    ///
    /// An empty target installs the immutable Arc payload and its verified
    /// Version cell directly. This is the cold-start handoff: Replica can prove
    /// durable material once, then populate its long-lived Engine without
    /// importing the same full snapshot a second time. If this Engine already
    /// has collaborative state, ordinary CRDT import performs the required
    /// merge; exact duplicates remain allocation-free and unchanged.
    pub fn import_verified_snapshot(
        &mut self,
        key: &Key,
        snapshot: &BodySnapshot,
    ) -> Result<crate::causal::ImportStatus, Failure> {
        use crate::causal::ImportStatus;

        snapshot
            .version()
            .map_err(|_| Failure::Invalid(commit::Invalid::Import))?;
        match &snapshot.export {
            SnapshotExport::Atomic(bytes) => {
                if matches!(
                    self.bodies.get(key),
                    Some(BodyState::Collab(_) | BodyState::FrozenCollab(_))
                ) {
                    return Err(Failure::TypeConflict);
                }
                let applied = !matches!(
                    self.bodies.get(key),
                    Some(BodyState::Atomic(current)) if current.as_ref() == bytes.as_ref()
                );
                if applied {
                    self.bodies
                        .insert(key.clone(), BodyState::Atomic(bytes.clone()));
                }
                Ok(ImportStatus {
                    applied,
                    pending: false,
                })
            }
            SnapshotExport::Collaborative(bytes) => {
                if matches!(self.bodies.get(key), Some(BodyState::Atomic(_))) {
                    return Err(Failure::TypeConflict);
                }
                if self.bodies.get(key).is_none() {
                    let version = snapshot
                        .version
                        .as_ref()
                        .ok_or(Failure::Invalid(commit::Invalid::Import))?
                        .clone();
                    self.bodies.insert(
                        key.clone(),
                        BodyState::FrozenCollab(FrozenCollab {
                            export: bytes.clone(),
                            version,
                        }),
                    );
                    return Ok(ImportStatus {
                        applied: true,
                        pending: false,
                    });
                }
                if matches!(
                    self.bodies.get(key),
                    Some(BodyState::FrozenCollab(current))
                        if current.export.as_ref() == bytes.as_ref()
                ) {
                    return Ok(ImportStatus {
                        applied: false,
                        pending: false,
                    });
                }
                let doc = self.collab_doc(key, true)?.ok_or(Failure::TypeConflict)?;
                let before = doc.oplog_frontiers();
                let status = doc
                    .import(bytes.as_ref())
                    .map_err(|_| Failure::Invalid(commit::Invalid::Merge))?;
                Ok(ImportStatus {
                    applied: before != doc.oplog_frontiers(),
                    pending: status.pending.is_some(),
                })
            }
        }
    }

    pub fn import_body(
        &mut self,
        key: &Key,
        export: &BodyExport,
    ) -> Result<Option<Receipt>, Failure> {
        let thaw = matches!(
            (self.bodies.get(key), export),
            (
                Some(BodyState::FrozenCollab(frozen)),
                BodyExport::Collaborative(snapshot)
            ) if frozen.export.as_ref() != snapshot.as_slice()
        );
        if thaw {
            self.make_hot(key)?;
        }
        let changed = match (self.bodies.get(key), export) {
            // Atomic replacement — policy for concurrent atomic writes is
            // Replica's, decided before this call.
            (Some(BodyState::Atomic(current)), BodyExport::Atomic(bytes)) => {
                if current.as_ref() == bytes.as_slice() {
                    false
                } else {
                    self.bodies
                        .insert(key.clone(), BodyState::Atomic(bytes.clone().into()));
                    true
                }
            }
            (None, BodyExport::Atomic(bytes)) => {
                self.bodies
                    .insert(key.clone(), BodyState::Atomic(bytes.clone().into()));
                true
            }
            // Collaborative causal merge: already-known material is unchanged.
            (Some(BodyState::Collab(doc)), BodyExport::Collaborative(snapshot)) => {
                let before = doc.oplog_frontiers().encode();
                doc.import(snapshot)
                    .map_err(|_| Failure::Invalid(commit::Invalid::Merge))?;
                doc.oplog_frontiers().encode() != before
            }
            (Some(BodyState::FrozenCollab(frozen)), BodyExport::Collaborative(snapshot)) => {
                frozen.export.as_ref() != snapshot.as_slice()
            }
            (None, BodyExport::Collaborative(snapshot)) => {
                let frozen = FrozenCollab::new(snapshot.clone().into());
                // Validate the interchange boundary before installing it. The
                // verified Version is retained in the same compact cell shared
                // by future read snapshots; malformed bytes never become an
                // empty causal coordinate.
                frozen.version()?;
                self.bodies
                    .insert(key.clone(), BodyState::FrozenCollab(frozen));
                true
            }
            // A model mismatch at the same key is a type conflict, refused.
            (Some(BodyState::Atomic(_)), BodyExport::Collaborative(_))
            | (Some(BodyState::Collab(_) | BodyState::FrozenCollab(_)), BodyExport::Atomic(_)) => {
                return Err(Failure::TypeConflict)
            }
        };
        if !changed {
            return Ok(None);
        }
        let mut touched = std::collections::BTreeSet::new();
        touched.insert(key.clone());
        Ok(Some(Receipt::new(self.causal_for(&touched)?, 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cold_checkpoint_seed_defers_import_until_worker_export() {
        let mut engine = Engine::new();
        let key = Key::from_bytes(b"body/cold-checkpoint".to_vec());
        engine
            .commit(Transaction::new(
                "seed",
                vec![Op::RegisterSet {
                    key: key.clone(),
                    path: "title".into(),
                    value: vec![b'x'; 1024 * 1024],
                }],
            ))
            .unwrap();
        let export = match engine.export_body(&key).unwrap() {
            BodyExport::Collaborative(bytes) => Arc::<[u8]>::from(bytes),
            BodyExport::Atomic(_) => panic!("collaborative fixture"),
        };
        let frozen = FrozenCollab::new(export);
        let version = Arc::clone(&frozen.version);
        engine
            .bodies
            .insert(key.clone(), BodyState::FrozenCollab(frozen));

        let seed = engine.checkpoint_seed(&key).unwrap();
        assert!(
            version.get().is_none(),
            "capturing a cold seed must not import the retained snapshot"
        );
        let artifact = seed.export().unwrap();
        assert!(
            version.get().is_some(),
            "the worker-side export may discover and memoize the version"
        );
        assert!(matches!(
            artifact,
            crate::causal::Artifact::Checkpoint { .. }
        ));
    }

    #[test]
    fn a_body_export_carries_exactly_one_body() {
        // Two collaborative Bodies; exporting one and importing it elsewhere
        // must bring that Body only — never a cross-Body snapshot.
        let mut a = Engine::new();
        let k1 = Key::from_bytes(b"body/1".to_vec());
        let k2 = Key::from_bytes(b"body/2".to_vec());
        a.commit(Transaction::new(
            "created",
            vec![
                Op::RegisterSet {
                    key: k1.clone(),
                    path: "title".into(),
                    value: b"one".to_vec(),
                },
                Op::RegisterSet {
                    key: k2.clone(),
                    path: "title".into(),
                    value: b"two".to_vec(),
                },
            ],
        ))
        .unwrap();

        let export = a.export_body(&k1).unwrap();
        assert!(matches!(export, BodyExport::Collaborative(_)));
        let mut b = Engine::new();
        b.import_body(&k1, &export).unwrap().unwrap();
        assert_eq!(
            b.read_collaborative(&k1).unwrap().registers["title"],
            b"one".to_vec()
        );
        assert!(
            b.read_collaborative(&k2).is_err() && b.read(&k2).is_none(),
            "the second Body did not ride along"
        );
    }

    #[test]
    fn per_body_import_preserves_stable_element_identity() {
        let mut a = Engine::new();
        let k = Key::from_bytes(b"body/ids".to_vec());
        a.commit(Transaction::new(
            "created",
            vec![Op::ListInsert {
                key: k.clone(),
                path: "items".into(),
                index: 0,
                value: b"x".to_vec(),
            }],
        ))
        .unwrap();
        let element = a.read_collaborative(&k).unwrap().lists["items"][0]
            .element
            .clone();

        // B imports the Body and removes the element BY THE SAME STABLE ID.
        let mut b = Engine::new();
        b.import_body(&k, &a.export_body(&k).unwrap()).unwrap();
        b.commit(Transaction::new(
            "removed",
            vec![Op::ListRemove {
                key: k.clone(),
                path: "items".into(),
                element,
            }],
        ))
        .unwrap();
        assert!(b.read_collaborative(&k).unwrap().lists["items"].is_empty());
    }

    #[test]
    fn reimporting_known_material_is_unchanged() {
        let mut a = Engine::new();
        let k = Key::from_bytes(b"body/known".to_vec());
        a.commit(Transaction::new(
            "created",
            vec![Op::CounterAdd {
                key: k.clone(),
                path: "votes".into(),
                delta: 2,
            }],
        ))
        .unwrap();
        let export = a.export_body(&k).unwrap();
        let mut b = Engine::new();
        assert!(b.import_body(&k, &export).unwrap().is_some(), "new");
        assert!(
            b.import_body(&k, &export).unwrap().is_none(),
            "already known — no receipt, nothing changed"
        );
        // Atomic idempotence too.
        let ak = Key::from_bytes(b"body/atomic".to_vec());
        let atomic = BodyExport::Atomic(b"v1".to_vec());
        assert!(b.import_body(&ak, &atomic).unwrap().is_some());
        assert!(b.import_body(&ak, &atomic).unwrap().is_none());
    }

    #[test]
    fn a_model_mismatch_at_the_same_key_is_a_type_conflict() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/mismatch".to_vec());
        f.commit(Transaction::new(
            "created",
            vec![Op::PutCanonical {
                key: k.clone(),
                value: b"atomic".to_vec(),
            }],
        ))
        .unwrap();
        // A collaborative export addressed at an atomic key is refused.
        let mut other = Engine::new();
        other
            .commit(Transaction::new(
                "created",
                vec![Op::CounterAdd {
                    key: k.clone(),
                    path: "votes".into(),
                    delta: 1,
                }],
            ))
            .unwrap();
        let collab = other.export_body(&k).unwrap();
        assert_eq!(
            f.import_body(&k, &collab).unwrap_err(),
            Failure::TypeConflict
        );
        assert_eq!(f.read(&k).as_deref(), Some(&b"atomic"[..]), "unchanged");
    }

    /// The shape the rest of the crate reads a hierarchy through: pre-order,
    /// parents named, values and entries separated, deleted subtrees gone.
    #[test]
    fn a_tree_projects_in_preorder_with_parents_values_and_entries() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/tree".to_vec());
        macro_rules! tree {
            ($ops:expr) => {
                f.commit(Transaction::new("threaded", $ops)).unwrap()
            };
        }

        tree!(vec![Op::TreeInsert {
            key: k.clone(),
            path: "comments".into(),
            parent: None,
            after: None,
            value: b"first".to_vec(),
        }]);
        let root = f.read_collaborative(&k).unwrap().trees["comments"][0]
            .node
            .clone();
        tree!(vec![
            Op::TreeInsert {
                key: k.clone(),
                path: "comments".into(),
                parent: Some(root.clone()),
                after: None,
                value: b"reply".to_vec(),
            },
            Op::TreeInsert {
                key: k.clone(),
                path: "comments".into(),
                parent: None,
                after: None,
                value: b"second".to_vec(),
            },
        ]);

        let nodes = f.read_collaborative(&k).unwrap().trees["comments"].clone();
        let payloads: Vec<&[u8]> = nodes.iter().map(|n| n.value.as_slice()).collect();
        assert_eq!(
            payloads,
            vec![&b"first"[..], &b"reply"[..], &b"second"[..]],
            "pre-order: a reply follows its parent, before the next root"
        );
        assert_eq!(nodes[0].parent, None);
        assert_eq!(nodes[1].parent.as_deref(), Some(root.as_str()));
        assert_eq!(nodes[2].parent, None);

        // A data entry lands beside the value, never on top of it.
        tree!(vec![Op::TreeSet {
            key: k.clone(),
            path: "comments".into(),
            node: root.clone(),
            entry: "pinned".into(),
            value: b"1".to_vec(),
        }]);
        let nodes = f.read_collaborative(&k).unwrap().trees["comments"].clone();
        assert_eq!(nodes[0].entries["pinned"], b"1".to_vec());
        assert_eq!(nodes[0].value, b"first".to_vec(), "value untouched");

        // Removing a node removes what hung under it.
        tree!(vec![Op::TreeRemove {
            key: k.clone(),
            path: "comments".into(),
            node: root,
        }]);
        let nodes = f.read_collaborative(&k).unwrap().trees["comments"].clone();
        assert_eq!(
            nodes.iter().map(|n| n.value.clone()).collect::<Vec<_>>(),
            vec![b"second".to_vec()],
            "the reply went with its parent"
        );
    }

    /// The two placements a caller can get wrong, and the value key it cannot
    /// have. Each refuses the whole batch rather than resolving into a
    /// hierarchy nobody asked for.
    #[test]
    fn tree_placement_cycles_and_the_reserved_entry_are_refused() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/tree-refusals".to_vec());
        let insert = |parent: Option<String>| Op::TreeInsert {
            key: k.clone(),
            path: "t".into(),
            parent,
            after: None,
            value: b"x".to_vec(),
        };
        f.commit(Transaction::new("seed", vec![insert(None)]))
            .unwrap();
        let a = f.read_collaborative(&k).unwrap().trees["t"][0].node.clone();
        f.commit(Transaction::new("seed", vec![insert(Some(a.clone()))]))
            .unwrap();
        let child = f.read_collaborative(&k).unwrap().trees["t"][1].node.clone();

        // A node cannot become its own descendant's child.
        assert_eq!(
            f.commit(Transaction::new(
                "cycle",
                vec![Op::TreeMove {
                    key: k.clone(),
                    path: "t".into(),
                    node: a.clone(),
                    parent: Some(child.clone()),
                    after: None,
                }]
            ))
            .unwrap_err(),
            Failure::Invalid(commit::Invalid::TreeCycle)
        );

        // `after` names a sibling; a node under a different parent is not one.
        assert_eq!(
            f.commit(Transaction::new(
                "placement",
                vec![
                    insert(None),
                    Op::TreeMove {
                        key: k.clone(),
                        path: "t".into(),
                        node: child.clone(),
                        parent: None,
                        after: Some(child.clone()),
                    }
                ]
            ))
            .unwrap_err(),
            Failure::Invalid(commit::Invalid::TreePlacement)
        );

        assert_eq!(
            f.commit(Transaction::new(
                "reserved",
                vec![Op::TreeSet {
                    key: k.clone(),
                    path: "t".into(),
                    node: a.clone(),
                    entry: NODE_VALUE_KEY.into(),
                    value: b"stolen".to_vec(),
                }]
            ))
            .unwrap_err(),
            Failure::Invalid(commit::Invalid::TreeReservedEntry)
        );

        // Every refusal rolled its whole batch back: two nodes, first value
        // intact.
        let nodes = f.read_collaborative(&k).unwrap().trees["t"].clone();
        assert_eq!(
            nodes.len(),
            2,
            "the failed insert rolled back with its batch"
        );
        assert_eq!(nodes[0].value, b"x".to_vec());
    }

    /// The whole reason the anchor form exists: one operation, no prior read,
    /// and neither record needs a node yet.
    #[test]
    fn an_anchored_placement_creates_both_ends_in_one_operation() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/anchored".to_vec());
        f.commit(Transaction::new(
            "parented",
            vec![Op::TreeAnchor {
                key: k.clone(),
                path: "hierarchy".into(),
                anchor: "iss_child".into(),
                parent: Some("iss_parent".into()),
            }],
        ))
        .unwrap();

        let nodes = f.read_collaborative(&k).unwrap().trees["hierarchy"].clone();
        assert_eq!(nodes.len(), 2, "both ends exist after one operation");
        assert_eq!(nodes[0].anchor.as_deref(), Some("iss_parent"));
        assert_eq!(nodes[1].anchor.as_deref(), Some("iss_child"));
        assert_eq!(nodes[1].parent.as_deref(), Some(nodes[0].node.as_str()));

        // Idempotent: asking again neither duplicates a node nor re-records a
        // move.
        let before = f.version(&k).unwrap();
        f.commit(Transaction::new(
            "parented",
            vec![Op::TreeAnchor {
                key: k.clone(),
                path: "hierarchy".into(),
                anchor: "iss_child".into(),
                parent: Some("iss_parent".into()),
            }],
        ))
        .unwrap();
        assert_eq!(
            f.read_collaborative(&k).unwrap().trees["hierarchy"].len(),
            2
        );
        assert_eq!(f.version(&k).unwrap(), before, "no history for a no-op");

        // Unparenting returns it to a root without touching its own subtree.
        f.commit(Transaction::new(
            "unparented",
            vec![Op::TreeAnchor {
                key: k.clone(),
                path: "hierarchy".into(),
                anchor: "iss_child".into(),
                parent: None,
            }],
        ))
        .unwrap();
        let nodes = f.read_collaborative(&k).unwrap().trees["hierarchy"].clone();
        assert!(nodes.iter().all(|n| n.parent.is_none()), "both are roots");
    }

    /// The cycle the product used to check for against its own local view, now
    /// refused by the engine on whichever replica asks.
    #[test]
    fn an_anchored_placement_refuses_a_cycle_and_refuses_itself() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/anchor-cycle".to_vec());
        let place = |anchor: &str, parent: Option<&str>| Op::TreeAnchor {
            key: k.clone(),
            path: "h".into(),
            anchor: anchor.into(),
            parent: parent.map(str::to_string),
        };
        f.commit(Transaction::new("parented", vec![place("b", Some("a"))]))
            .unwrap();
        assert_eq!(
            f.commit(Transaction::new("parented", vec![place("a", Some("b"))]))
                .unwrap_err(),
            Failure::Invalid(commit::Invalid::TreeCycle)
        );
        assert_eq!(
            f.commit(Transaction::new("parented", vec![place("a", Some("a"))]))
                .unwrap_err(),
            Failure::Invalid(commit::Invalid::TreePlacement)
        );
    }

    /// What the type is for: state stops growing, and the count does not lie
    /// about it.
    #[test]
    fn a_log_bounds_its_tail_and_keeps_an_exact_count() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/log".to_vec());
        for i in 0..50u8 {
            f.commit(Transaction::new(
                "appended",
                vec![Op::LogAppend {
                    key: k.clone(),
                    path: "events".into(),
                    value: vec![i],
                    retain: 8,
                }],
            ))
            .unwrap();
        }
        let log = f.read_collaborative(&k).unwrap().logs["events"].clone();
        assert_eq!(log.entries.len(), 8, "state is bounded by the retention");
        assert_eq!(log.appended, 50, "the count is of everything, not the tail");
        assert_eq!(
            log.entries.iter().map(|e| e.value[0]).collect::<Vec<_>>(),
            (42..50).collect::<Vec<u8>>(),
            "the tail is the newest entries, oldest trimmed first"
        );
        // Entry identity is stable and is what a cursor holds — a position
        // cannot be, because trimming renumbers everything behind it.
        let ids: std::collections::BTreeSet<&str> =
            log.entries.iter().map(|e| e.element.as_str()).collect();
        assert_eq!(ids.len(), 8, "every retained entry has its own identity");

        // A retention of zero is a log that cannot be read; refused, and the
        // batch changes nothing.
        assert_eq!(
            f.commit(Transaction::new(
                "appended",
                vec![Op::LogAppend {
                    key: k.clone(),
                    path: "events".into(),
                    value: vec![99],
                    retain: 0,
                }]
            ))
            .unwrap_err(),
            Failure::Invalid(commit::Invalid::Bounds)
        );
        let after = f.read_collaborative(&k).unwrap().logs["events"].clone();
        assert_eq!(after.appended, 50, "the refused append did not count");
        assert_eq!(after.entries.len(), 8);
    }

    /// A log is a type like any other at a path.
    #[test]
    fn a_log_path_conflicts_with_the_other_types() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/log-conflict".to_vec());
        f.commit(Transaction::new(
            "seed",
            vec![Op::LogAppend {
                key: k.clone(),
                path: "feed".into(),
                value: b"x".to_vec(),
                retain: 4,
            }],
        ))
        .unwrap();
        assert_eq!(
            f.commit(Transaction::new(
                "conflict",
                vec![Op::ListInsert {
                    key: k.clone(),
                    path: "feed".into(),
                    index: 0,
                    value: b"x".to_vec(),
                }]
            ))
            .unwrap_err(),
            Failure::TypeConflict
        );
    }

    /// A hierarchy is a type like any other: a path bound to it refuses the
    /// other six, and they refuse it.
    #[test]
    fn a_tree_path_conflicts_with_the_other_types() {
        let mut f = Engine::new();
        let k = Key::from_bytes(b"body/tree-conflict".to_vec());
        f.commit(Transaction::new(
            "seed",
            vec![Op::TreeInsert {
                key: k.clone(),
                path: "threads".into(),
                parent: None,
                after: None,
                value: b"x".to_vec(),
            }],
        ))
        .unwrap();
        assert_eq!(
            f.commit(Transaction::new(
                "conflict",
                vec![Op::ListInsert {
                    key: k.clone(),
                    path: "threads".into(),
                    index: 0,
                    value: b"x".to_vec(),
                }]
            ))
            .unwrap_err(),
            Failure::TypeConflict
        );
        // And the reverse direction, at a path a list already holds.
        f.commit(Transaction::new(
            "seed",
            vec![Op::ListInsert {
                key: k.clone(),
                path: "flat".into(),
                index: 0,
                value: b"x".to_vec(),
            }],
        ))
        .unwrap();
        assert_eq!(
            f.commit(Transaction::new(
                "conflict",
                vec![Op::TreeInsert {
                    key: k.clone(),
                    path: "flat".into(),
                    parent: None,
                    after: None,
                    value: b"x".to_vec(),
                }]
            ))
            .unwrap_err(),
            Failure::TypeConflict
        );
    }

    #[test]
    fn transaction_request_roundtrips_postcard() {
        let req = Transaction::new(
            "created",
            vec![
                Op::PutCanonical {
                    key: Key::from_bytes(vec![1, 2, 3]),
                    value: vec![9],
                },
                Op::Remove {
                    key: Key::from_bytes(vec![4]),
                },
            ],
        );
        let bytes = postcard::to_stdvec(&req).unwrap();
        let back: Transaction = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn atomic_bodies_commit_read_remove_and_advance_the_causal_token() {
        let mut fabric = Engine::new();
        let key = Key::from_bytes(b"body/0".to_vec());
        let r1 = fabric
            .commit(Transaction::new(
                "created",
                vec![Op::PutCanonical {
                    key: key.clone(),
                    value: b"v1".to_vec(),
                }],
            ))
            .unwrap();
        assert_eq!(r1.applied(), 1);
        assert_eq!(fabric.read(&key).as_deref(), Some(&b"v1"[..]));

        let r2 = fabric
            .commit(Transaction::new(
                "removed",
                vec![Op::Remove { key: key.clone() }],
            ))
            .unwrap();
        // The causal token advances between commits.
        assert_ne!(r1.causal(), r2.causal());
        assert_eq!(fabric.read(&key), None);
    }

    #[test]
    fn prepared_candidate_is_readable_and_rolls_back_only_touched_bodies() {
        let mut fabric = Engine::new();
        let changed = Key::from_bytes(b"body/changed".to_vec());
        let untouched = Key::from_bytes(b"body/untouched".to_vec());
        fabric
            .commit(Transaction::new(
                "seed",
                vec![
                    Op::PutCanonical {
                        key: changed.clone(),
                        value: b"old".to_vec(),
                    },
                    Op::PutCanonical {
                        key: untouched.clone(),
                        value: b"stable".to_vec(),
                    },
                ],
            ))
            .unwrap();

        let prepared = fabric
            .prepare(Transaction::new(
                "candidate",
                vec![Op::PutCanonical {
                    key: changed.clone(),
                    value: b"candidate".to_vec(),
                }],
            ))
            .unwrap();
        assert_eq!(fabric.read(&changed).as_deref(), Some(&b"candidate"[..]));
        assert_eq!(fabric.read(&untouched).as_deref(), Some(&b"stable"[..]));
        fabric.rollback(prepared).unwrap();
        assert_eq!(fabric.read(&changed).as_deref(), Some(&b"old"[..]));
        assert_eq!(fabric.read(&untouched).as_deref(), Some(&b"stable"[..]));
    }

    #[test]
    fn immutable_snapshots_share_atomic_and_frozen_collaborative_payloads() {
        let mut fabric = Engine::new();
        let atomic = Key::from_bytes(b"body/atomic-shared".to_vec());
        fabric
            .commit(Transaction::new(
                "atomic",
                vec![Op::PutCanonical {
                    key: atomic.clone(),
                    value: vec![7; 4096],
                }],
            ))
            .unwrap();
        let atomic_engine = match fabric.bodies.get(&atomic).unwrap() {
            BodyState::Atomic(bytes) => bytes.clone(),
            _ => panic!("atomic Body changed model"),
        };
        let atomic_snapshot = fabric.body_snapshot(&atomic).unwrap().unwrap();
        let SnapshotExport::Atomic(atomic_read) = &atomic_snapshot.export else {
            panic!("atomic snapshot changed model");
        };
        assert!(Arc::ptr_eq(&atomic_engine, atomic_read));

        let collaborative = Key::from_bytes(b"body/collaborative-shared".to_vec());
        let mut source = Engine::new();
        source
            .commit(Transaction::new(
                "text",
                vec![Op::TextSplice {
                    key: collaborative.clone(),
                    path: "description".into(),
                    index: 0,
                    delete: 0,
                    insert: "shared".into(),
                }],
            ))
            .unwrap();
        fabric
            .import_body(&collaborative, &source.export_body(&collaborative).unwrap())
            .unwrap();
        let frozen = match fabric.bodies.get(&collaborative).unwrap() {
            BodyState::FrozenCollab(frozen) => frozen.clone(),
            _ => panic!("import retained a live collaborative document"),
        };
        let collaborative_snapshot = fabric.body_snapshot(&collaborative).unwrap().unwrap();
        let SnapshotExport::Collaborative(collaborative_read) = &collaborative_snapshot.export
        else {
            panic!("collaborative snapshot changed model");
        };
        assert!(Arc::ptr_eq(&frozen.export, collaborative_read));
        assert!(Arc::ptr_eq(
            &frozen.version,
            collaborative_snapshot
                .version
                .as_ref()
                .expect("collaborative snapshot carries a version cell")
        ));
    }

    #[test]
    fn read_only_projection_does_not_inflate_a_frozen_body() {
        let key = Key::from_bytes(b"body/read-cold".to_vec());
        let mut source = Engine::new();
        source
            .commit(Transaction::new(
                "text",
                vec![Op::TextSplice {
                    key: key.clone(),
                    path: "description".into(),
                    index: 0,
                    delete: 0,
                    insert: "cold".into(),
                }],
            ))
            .unwrap();
        let mut replica = Engine::new();
        replica
            .import_body(&key, &source.export_body(&key).unwrap())
            .unwrap();

        assert!(replica.hot.is_empty());
        assert!(matches!(
            replica.bodies.get(&key),
            Some(BodyState::FrozenCollab(_))
        ));
        assert_eq!(
            replica.read_collaborative(&key).unwrap().texts["description"],
            "cold"
        );
        assert!(replica.hot.is_empty());
        assert!(matches!(
            replica.bodies.get(&key),
            Some(BodyState::FrozenCollab(_))
        ));
    }

    #[test]
    fn verified_snapshot_handoff_shares_payload_without_a_second_import() {
        let key = Key::from_bytes(b"body/recovered".to_vec());
        let mut proof = Engine::new();
        proof
            .commit(Transaction::new(
                "text",
                vec![Op::TextSplice {
                    key: key.clone(),
                    path: "description".into(),
                    index: 0,
                    delete: 0,
                    insert: "verified".into(),
                }],
            ))
            .unwrap();
        let verified = proof.body_snapshot(&key).unwrap().unwrap();
        let expected_version = verified.version().unwrap();

        let mut recovered = Engine::new();
        let status = recovered.import_verified_snapshot(&key, &verified).unwrap();
        assert!(status.applied);
        assert!(!status.pending);
        let frozen = match recovered.bodies.get(&key).unwrap() {
            BodyState::FrozenCollab(frozen) => frozen,
            _ => panic!("verified recovery inflated a live document"),
        };
        let SnapshotExport::Collaborative(expected_export) = &verified.export else {
            panic!("proof snapshot changed model");
        };
        assert!(Arc::ptr_eq(&frozen.export, expected_export));
        assert!(Arc::ptr_eq(
            &frozen.version,
            verified
                .version
                .as_ref()
                .expect("collaborative snapshot carries a version cell")
        ));
        assert_eq!(recovered.version(&key).unwrap(), expected_version);
        assert!(recovered.hot.is_empty());
    }

    #[test]
    fn repeated_edits_reuse_one_hot_doc_and_lru_bounds_all_live_docs() {
        let mut fabric = Engine::new();
        let repeated = Key::from_bytes(b"body/repeated".to_vec());
        fabric
            .commit(Transaction::new(
                "seed",
                vec![Op::TextSplice {
                    key: repeated.clone(),
                    path: "description".into(),
                    index: 0,
                    delete: 0,
                    insert: "x".into(),
                }],
            ))
            .unwrap();
        let first_doc = match fabric.bodies.get(&repeated).unwrap() {
            BodyState::Collab(doc) => doc as *const loro::LoroDoc,
            _ => panic!("finalize cooled the mutation-hot Body"),
        };
        for index in 1..8u64 {
            fabric
                .commit(Transaction::new(
                    "edit",
                    vec![Op::TextSplice {
                        key: repeated.clone(),
                        path: "description".into(),
                        index,
                        delete: 0,
                        insert: "x".into(),
                    }],
                ))
                .unwrap();
            let next_doc = match fabric.bodies.get(&repeated).unwrap() {
                BodyState::Collab(doc) => doc as *const loro::LoroDoc,
                _ => panic!("a repeated edit re-froze its live document"),
            };
            assert_eq!(first_doc, next_doc, "the live LoroDoc was replaced");
        }

        for ordinal in 0..(MAX_HOT_COLLABORATIVE_BODIES + 17) {
            let key = Key::from_bytes(format!("body/lru/{ordinal:04}").into_bytes());
            fabric
                .commit(Transaction::new(
                    "edit",
                    vec![Op::TextSplice {
                        key,
                        path: "description".into(),
                        index: 0,
                        delete: 0,
                        insert: "x".into(),
                    }],
                ))
                .unwrap();
        }
        let live = fabric
            .bodies
            .values()
            .filter(|body| matches!(body, BodyState::Collab(_)))
            .count();
        assert_eq!(fabric.hot.len(), MAX_HOT_COLLABORATIVE_BODIES);
        assert_eq!(live, MAX_HOT_COLLABORATIVE_BODIES);
        assert!(matches!(
            fabric.bodies.get(&repeated),
            Some(BodyState::FrozenCollab(_))
        ));
    }

    #[test]
    fn malformed_collaborative_snapshot_never_becomes_an_empty_version() {
        let key = Key::from_bytes(b"body/malformed".to_vec());
        let snapshot =
            BodySnapshot::from_export(&key, BodyExport::Collaborative(vec![0xff, 0x00, 0x7f]))
                .unwrap();
        assert!(snapshot.version().is_err());

        let mut fabric = Engine::new();
        assert!(fabric.import_verified_snapshot(&key, &snapshot).is_err());
        assert!(fabric
            .import_body(&key, &BodyExport::Collaborative(vec![0xff, 0x00, 0x7f]),)
            .is_err());
        assert!(!fabric.bodies.contains_key(&key));
    }
}
