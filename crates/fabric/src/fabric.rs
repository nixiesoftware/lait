//! The Fabric operation surface and engine — the sealed contract Replica drives.
//!
//! Fabric is LAIT's canonical, sealed Loro component and the only crate that
//! names Loro. It exposes **LAIT-owned** semantic operations and results, never
//! raw documents, containers, or Loro frontier types. Replica validates and
//! constructs a semantic transaction plan, submits it to a Fabric-owned
//! [`Fabric`] engine, and advances its semantic frontier only from a durable
//! [`FabricCommitReceipt`]. Fabric never imports Replica.
//!
//! **Ownership boundary (enforced, not just documented):**
//! - Replica submits *semantic* [`FabricOp`]s — it never authors a Loro delta.
//!   The concrete translation to Loro is Fabric-private.
//! - [`FabricCommitReceipt`] and [`CausalToken`] can be constructed **only**
//!   inside this crate (their constructors are `pub(crate)`), so a receipt is
//!   proof of a real Fabric commit — an outside crate cannot forge the token
//!   Replica advances from.
//!
//! [`CrdtFabric`] is the sole engine: atomic Bodies plus the frozen
//! collaborative algebra (register/map/list/text/set/counter with stable
//! element identity) over real Loro containers — one Loro document per Body,
//! so per-Body export/import carries exactly one Body's causal history — with
//! batch atomicity. The durable store is the journaled protocol in
//! [`crate::journal`], persisting per-Body protected objects.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// An opaque commitment to Fabric's internal causal position (Loro frontier),
/// carried as bytes. It rides inside [`FabricCommitReceipt`] and is never
/// interpreted outside Fabric — no `loro::*` type crosses the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CausalToken(Vec<u8>);

impl CausalToken {
    /// Construct a causal token. **Crate-private**: only the Fabric engine mints
    /// one, so a token always denotes a real Fabric position.
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A key into the Fabric representation — an opaque handle Replica uses to
/// address a durable object without naming a Loro container. Its concrete
/// encoding is Fabric-private.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FabricKey(Vec<u8>);

impl FabricKey {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A single Fabric-level **semantic** operation. Replica alone translates a
/// semantic `BodyOp` into one of these; Fabric maps them canonically onto Loro.
/// Replica never authors a raw Loro delta — that is the ownership boundary.
///
/// The collaborative operations implement the frozen S1 algebra: each addresses
/// a `path` inside one collaborative Body (`key`), a path is bound to exactly
/// one collaborative type for the Body's lifetime (a second type at the same
/// path is a [`FabricError::TypeConflict`]), list elements carry **stable
/// element ids** minted by Fabric at insert time (never indices), sets are
/// add-wins (observed-remove), counters sum all increments, and text splices
/// use Unicode-scalar coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FabricOp {
    /// Atomically replace the canonical bytes stored at a key.
    PutCanonical { key: FabricKey, value: Vec<u8> },
    /// Remove the object at a key (atomic value or whole collaborative Body).
    Remove { key: FabricKey },
    /// Ensure a collaborative Body root exists at a key (Body create).
    CreateBody { key: FabricKey },
    /// Last-writer-wins register set.
    RegisterSet {
        key: FabricKey,
        path: String,
        value: Vec<u8>,
    },
    /// Clear a register.
    RegisterClear { key: FabricKey, path: String },
    /// Map entry set (LWW per entry).
    MapSet {
        key: FabricKey,
        path: String,
        entry: String,
        value: Vec<u8>,
    },
    /// Map entry remove.
    MapRemove {
        key: FabricKey,
        path: String,
        entry: String,
    },
    /// Ordered-list insert at a position; Fabric mints the stable element id.
    ListInsert {
        key: FabricKey,
        path: String,
        index: u64,
        value: Vec<u8>,
    },
    /// Ordered-list remove **by stable element id**.
    ListRemove {
        key: FabricKey,
        path: String,
        element: String,
    },
    /// Ordered-list move **by stable element id** to a position.
    ListMove {
        key: FabricKey,
        path: String,
        element: String,
        index: u64,
    },
    /// Text splice with Unicode-scalar coordinates.
    TextSplice {
        key: FabricKey,
        path: String,
        index: u64,
        delete: u64,
        insert: String,
    },
    /// Add-wins set add.
    SetAdd {
        key: FabricKey,
        path: String,
        value: Vec<u8>,
    },
    /// Set remove (removes the observed adds; a concurrent add survives).
    SetRemove {
        key: FabricKey,
        path: String,
        value: Vec<u8>,
    },
    /// Commutative counter increment.
    CounterAdd {
        key: FabricKey,
        path: String,
        delta: i64,
    },
}

/// A durable transaction request: an ordered batch of Fabric operations to apply
/// atomically, carrying the request/commit metadata Fabric labels the change
/// with in the oplog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FabricTransactionRequest {
    /// The semantic request label (e.g. `"created"`) surfaced in the oplog.
    pub request: String,
    pub ops: Vec<FabricOp>,
}

impl FabricTransactionRequest {
    pub fn new(request: impl Into<String>, ops: Vec<FabricOp>) -> Self {
        Self {
            request: request.into(),
            ops,
        }
    }
}

/// The receipt of a durable Fabric commit. Replica advances its semantic
/// frontier **only** from this. It carries the post-commit causal token and the
/// count of changes applied. Constructed only by the Fabric engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FabricCommitReceipt {
    causal: CausalToken,
    applied: u32,
}

impl FabricCommitReceipt {
    /// **Crate-private**: only the Fabric engine issues a receipt.
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

/// Why a Fabric commit failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FabricError {
    /// A durable write (or a rollback after a failed apply) failed. The engine
    /// state may have diverged from the store — the caller must fail stop.
    Durability(String),
    /// The engine does not support this operation. Reserved (the Loro engine
    /// supports the full algebra); the error surface stays stable.
    Unsupported,
    /// The operation's type disagrees with what its target is already bound to:
    /// a collaborative op on an atomic Body, an atomic put over a collaborative
    /// Body, or a second collaborative type at an already-bound path.
    TypeConflict,
    /// The operation was structurally invalid at apply time (out-of-bounds
    /// index, unknown element id, counter overflow). The batch is rolled back.
    InvalidOp(String),
    /// The durable store failed integrity validation (a manifest naming absent
    /// or corrupt objects, a corrupt journal, a missing transaction counter).
    /// Never repaired heuristically — recreation guidance is the caller's.
    Integrity(String),
    /// The authoritative switch happened but its durability confirmation
    /// failed: the commit may or may not survive power loss. Fail stop and
    /// reopen — recovery resolves the outcome deterministically from the
    /// on-disk manifest. Never retry through this error.
    OutcomeUnknown,
}

/// A canonical, Loro-free view of one collaborative Body, keyed by path. This
/// is what a World reads back through the bounded context: list elements expose
/// the **stable element ids** Fabric minted at insert (the handles `ListRemove`
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
}

/// One ordered-list element: its stable Fabric-minted id and its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListElement {
    pub element: String,
    pub value: Vec<u8>,
}

impl std::fmt::Display for FabricError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for FabricError {}

impl From<journal::JournalError> for FabricError {
    fn from(e: journal::JournalError) -> Self {
        match e {
            journal::JournalError::Durability(m) => FabricError::Durability(m),
            journal::JournalError::Integrity(m) => FabricError::Integrity(m),
            journal::JournalError::OutcomeUnknown => FabricError::OutcomeUnknown,
        }
    }
}

/// The Fabric engine: the durable, canonical collaborative representation
/// Replica drives. It accepts semantic operations and returns a receipt whose
/// construction is Fabric-private, serves committed reads, and exports/imports
/// **one Body at a time** — the canonical store persists per-Body protected
/// objects, never a whole-engine snapshot. [`CrdtFabric`] is the only engine.
pub trait Fabric {
    /// Durably apply a transaction and return a commit receipt. Atomic: either
    /// every op is applied and a receipt returned, or nothing changes.
    fn commit(
        &mut self,
        request: FabricTransactionRequest,
    ) -> Result<FabricCommitReceipt, FabricError>;

    /// Read the committed canonical bytes at a key, if present.
    fn read(&self, key: &FabricKey) -> Option<Vec<u8>>;

    /// Project the committed collaborative view of a Body.
    ///
    /// Fallible rather than optional, because "there is nothing here" and "there
    /// is something here I cannot read" are different answers and a caller acts
    /// differently on each. A Body carrying a collaborative type this build does
    /// not implement is [`ProjectionError::SchemaAhead`] — its bytes stay
    /// stored, forwarded, and converged, and no caller is handed a view that
    /// looks complete and is not.
    fn read_collaborative(&self, key: &FabricKey) -> Result<CollaborativeView, ProjectionError>;

    /// Export **one Body's** canonical representation, if the Body exists. A
    /// collaborative export preserves causal history and stable element
    /// identity for exactly that Body — never a whole-engine or cross-Body
    /// snapshot. This is the payload the protected Body object seals.
    fn export_body(&self, key: &FabricKey) -> Option<BodyExport>;

    /// This Body's current position in its collaborative history.
    fn version(
        &self,
        key: &FabricKey,
    ) -> Result<crate::causal::FabricVersion, crate::causal::CausalError>;

    /// Operations after `from`, as an independently importable artifact.
    ///
    /// `from` is always this replica's own prior version. It cannot be a remote
    /// claim: expanding a head set requires the history it names, so a diverged
    /// peer could not be answered anyway — which is why convergence exchanges
    /// artifacts rather than versions.
    fn export_delta(
        &self,
        key: &FabricKey,
        from: &crate::causal::FabricVersion,
    ) -> Result<crate::causal::FabricArtifact, crate::causal::CausalError>;

    /// Current state with history trimmed at `retention_frontier`.
    fn export_checkpoint(
        &self,
        key: &FabricKey,
        retention_frontier: &crate::causal::FabricVersion,
    ) -> Result<crate::causal::FabricArtifact, crate::causal::CausalError>;

    /// The complete history as it stands now — taken immediately before a trim,
    /// so the work the trim will refuse can still be readmitted later.
    fn export_history(
        &self,
        key: &FabricKey,
    ) -> Result<crate::causal::FabricArtifact, crate::causal::CausalError>;

    /// Import one artifact. Order-independent: material whose dependencies have
    /// not arrived is held pending rather than refused.
    fn import_artifact(
        &mut self,
        key: &FabricKey,
        artifact: &crate::causal::FabricArtifact,
    ) -> Result<crate::causal::ImportStatus, crate::causal::CausalError>;

    /// How two positions in this Body's history relate.
    fn relation(
        &self,
        key: &FabricKey,
        a: &crate::causal::FabricVersion,
        b: &crate::causal::FabricVersion,
    ) -> crate::causal::CausalRelation;

    /// Take an anchor at a position in a collaborative value.
    fn anchor(
        &self,
        key: &FabricKey,
        path: &str,
        position: u64,
    ) -> Result<crate::causal::FabricAnchor, crate::causal::CausalError>;

    /// Resolve an anchor against the Body's current state.
    ///
    /// Total, and never mutates: a position that cannot be mapped is `Drifted`,
    /// so this is safe on a read-only replica and a renderer is never handed a
    /// silently wrong index.
    fn resolve(
        &self,
        key: &FabricKey,
        anchor: &crate::causal::FabricAnchor,
    ) -> crate::causal::AnchorResolution;

    /// Import one Body's canonical exported representation, addressed to
    /// exactly the given key. A collaborative import merges causally (already
    /// -known material returns `None`); an atomic import replaces the value
    /// (`None` when byte-identical). Fabric applies the change as directed —
    /// legitimacy, ordering, and conflict policy for atomic replacement are
    /// the caller's (Replica's) to decide **before** calling.
    fn import_body(
        &mut self,
        key: &FabricKey,
        export: &BodyExport,
    ) -> Result<Option<FabricCommitReceipt>, FabricError>;
}

/// One Body's canonical exported representation: an atomic Body's canonical
/// application bytes, or a collaborative Body's canonical per-Body Fabric
/// export (causality and stable element identity preserved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyExport {
    Atomic(Vec<u8>),
    Collaborative(Vec<u8>),
}

/// The Loro-backed engine: the canonical collaborative representation, and the
/// reason this crate alone names Loro.
///
/// **Layout — one Loro document per Body.** Each collaborative Body is its own
/// `LoroDoc`, so its canonical export ([`Fabric::export_body`]) carries exactly
/// that Body's causal history and stable element identity — never a whole-
/// engine or cross-Body snapshot. Inside a Body's doc, one root map (`body`)
/// holds keys `"<type>:<path>"` — `reg:` LWW binary registers, `map:` child
/// maps of binary entries, `list:` child movable lists whose element values are
/// `element_id[16] || value` (the id embedded in the value is the **stable
/// element identity**, LAIT-owned and sync-stable), `text:` child texts
/// (Unicode-scalar splices), `set:` child maps implementing an observed-remove
/// set (`"<value-hash>:<unique-tag>"` per add, so a remove only deletes the
/// adds it observed — add-wins), and `cnt:` child maps implementing a
/// PN-counter (each doc session sums into its own peer key; concurrent
/// increments land in disjoint keys and always sum). An atomic Body is a plain
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
pub struct CrdtFabric {
    /// This activation's writer id, minted once and shared by every document
    /// this engine authors into. One id per activation rather than one per
    /// document keeps a Body's version vector growing with restarts rather than
    /// with Bodies, and never persisting it is what keeps a copied store from
    /// minting colliding operation ids.
    writer: u64,
    bodies: BTreeMap<FabricKey, BodyState>,
}

/// One Body's live state.
enum BodyState {
    Atomic(Vec<u8>),
    Collab(loro::LoroDoc),
}

const BODY_MAP: &str = "body";

/// Domain for the receipt's causal token digest.
const CAUSAL_DOMAIN: &[u8] = b"lait/fabric-causal/1";

/// The collaborative type tags a path can be bound to.
const TYPE_TAGS: [&str; 6] = ["reg", "map", "list", "text", "set", "cnt"];

/// Type tags reserved for collaborative types a later docket adds. Reserving
/// them costs nothing now and keeps a seventh type a `CollaborativeSchema`
/// version bump plus a fixture set, rather than a format migration — the
/// checkpoint and delta encodings freeze on top of this algebra, and after that
/// an unreserved tag is expensive.
///
/// The two named are the ones the product is already working around. `tree` is
/// a movable hierarchy with stable node ids: sub-issues, milestone nesting, and
/// threaded comments are all hierarchies, and Issues currently hand-encodes
/// threading through a `reply_to` field over flat storage, which gives
/// concurrent re-parenting no defined outcome. `log` is an append-only sequence
/// whose state function is *last N plus a count* — activity feeds are unbounded
/// Lists today, re-checkpointed in full on every append.
const RESERVED_TYPE_TAGS: [&str; 2] = ["tree", "log"];

/// Why a collaborative projection could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    /// No Body at this key, or the Body is atomic rather than collaborative.
    NotCollaborative,
    /// The Body binds a path to a collaborative type this build does not
    /// implement. Reserved-but-unimplemented tags land here, which is the
    /// point: schema gating upstream should have refused the Body already, and
    /// the projection layer is the wrong place to paper over that.
    SchemaAhead { tag: String },
    /// A known tag bound to a value shape it cannot hold. Corruption or a
    /// schema disagreement, not a version gap.
    Malformed { tag: String },
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectionError::NotCollaborative => write!(f, "not a collaborative Body"),
            ProjectionError::SchemaAhead { tag } => {
                write!(f, "collaborative type `{tag}` is ahead of this build")
            }
            ProjectionError::Malformed { tag } => {
                write!(f, "collaborative type `{tag}` holds an impossible value")
            }
        }
    }
}
impl std::error::Error for ProjectionError {}

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
    getrandom::fill(&mut raw).expect("getrandom");
    raw
}

/// The set-member key prefix for a value: 128 bits of BLAKE3 over the value.
fn set_member_prefix(value: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&blake3::hash(value).as_bytes()[..16])
}

/// A fresh per-Body doc with the crate's canonical Loro config, authoring as
/// `writer`.
fn new_body_doc(writer: Option<u64>) -> loro::LoroDoc {
    let doc = loro::LoroDoc::new();
    crate::op::configure(&doc, writer);
    doc
}

impl BodyState {
    /// A position digest for the causal token: the atomic bytes' hash, or the
    /// collaborative doc's oplog frontier.
    fn digest(&self) -> Vec<u8> {
        match self {
            BodyState::Atomic(bytes) => blake3::hash(bytes).as_bytes().to_vec(),
            BodyState::Collab(doc) => doc.oplog_frontiers().encode().to_vec(),
        }
    }

    fn export(&self) -> Result<BodyExport, FabricError> {
        match self {
            BodyState::Atomic(bytes) => Ok(BodyExport::Atomic(bytes.clone())),
            BodyState::Collab(doc) => doc
                .export(loro::ExportMode::Snapshot)
                .map(BodyExport::Collaborative)
                .map_err(|e| FabricError::Durability(format!("export body: {e}"))),
        }
    }

    fn from_export(export: &BodyExport) -> Result<Self, FabricError> {
        match export {
            BodyExport::Atomic(bytes) => Ok(BodyState::Atomic(bytes.clone())),
            BodyExport::Collaborative(snapshot) => {
                // A doc reconstructed from an export is not yet authoring, so
                // it keeps Loro's own id until this activation writes into it
                // through `collab_doc`.
                let doc = new_body_doc(None);
                doc.import(snapshot)
                    .map_err(|e| FabricError::InvalidOp(format!("import body: {e}")))?;
                Ok(BodyState::Collab(doc))
            }
        }
    }
}

impl CrdtFabric {
    /// A fresh, empty Loro-backed engine, minting this activation's writer id.
    pub fn new() -> Self {
        Self {
            writer: crate::op::mint_activation_peer(),
            bodies: BTreeMap::new(),
        }
    }

    /// This activation's writer id.
    pub fn writer(&self) -> u64 {
        self.writer
    }

    /// The keys of every present Body.
    pub fn body_keys(&self) -> Vec<FabricKey> {
        self.bodies.keys().cloned().collect()
    }

    fn loro_err(e: impl std::fmt::Display) -> FabricError {
        FabricError::InvalidOp(e.to_string())
    }

    /// The collaborative doc for a Body, creating it when `create`. An atomic
    /// value at the key is a [`FabricError::TypeConflict`].
    fn collab_doc(
        &mut self,
        key: &FabricKey,
        create: bool,
    ) -> Result<Option<&loro::LoroDoc>, FabricError> {
        use std::collections::btree_map::Entry;
        match self.bodies.entry(key.clone()) {
            Entry::Occupied(e) => match e.into_mut() {
                BodyState::Collab(doc) => Ok(Some(doc)),
                BodyState::Atomic(_) => Err(FabricError::TypeConflict),
            },
            Entry::Vacant(v) if create => {
                let BodyState::Collab(doc) =
                    v.insert(BodyState::Collab(new_body_doc(Some(self.writer))))
                else {
                    unreachable!()
                };
                Ok(Some(doc))
            }
            Entry::Vacant(_) => Ok(None),
        }
    }

    /// Enforce "a path is bound to exactly one collaborative type": no other
    /// type tag may already hold state at this path. Containers live at doc
    /// ROOTS (name-identified — see the struct docs); a register is a key in
    /// the `body` root map.
    fn check_path_type(doc: &loro::LoroDoc, tag: &str, path: &str) -> Result<(), FabricError> {
        let body = doc.get_map(BODY_MAP);
        for other in TYPE_TAGS {
            if other == tag {
                continue;
            }
            let name = typed_key(other, path);
            let bound = match other {
                "reg" => body.get(&name).is_some(),
                "list" => !doc.get_movable_list(name.as_str()).is_empty(),
                "text" => !doc.get_text(name.as_str()).is_empty(),
                _ => !doc.get_map(name.as_str()).is_empty(),
            };
            if bound {
                return Err(FabricError::TypeConflict);
            }
        }
        Ok(())
    }

    /// The Body's doc for a typed-path write, with the path-type binding
    /// enforced.
    fn doc_for(
        &mut self,
        key: &FabricKey,
        tag: &str,
        path: &str,
    ) -> Result<&loro::LoroDoc, FabricError> {
        let doc = self.collab_doc(key, true)?.expect("created on demand");
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
            let id = data_encoding::HEXLOWER.encode(&bytes[..ELEMENT_ID_LEN]);
            out.push((i, id, bytes[ELEMENT_ID_LEN..].to_vec()));
        }
        out
    }

    /// The causal token digesting the touched Bodies' post-commit positions.
    fn causal_for(&self, touched: &std::collections::BTreeSet<FabricKey>) -> CausalToken {
        let mut h = blake3::Hasher::new();
        h.update(CAUSAL_DOMAIN);
        for key in touched {
            h.update(&(key.as_bytes().len() as u64).to_le_bytes());
            h.update(key.as_bytes());
            match self.bodies.get(key) {
                Some(state) => {
                    let digest = state.digest();
                    h.update(&[1]);
                    h.update(&(digest.len() as u64).to_le_bytes());
                    h.update(&digest);
                }
                None => {
                    h.update(&[0]);
                }
            }
        }
        CausalToken::from_bytes(h.finalize().as_bytes().to_vec())
    }

    /// Apply one operation. Errors leave partially-applied state in the touched
    /// Body; [`Fabric::commit`] rolls the whole batch back from its backups.
    fn apply(&mut self, op: &FabricOp) -> Result<(), FabricError> {
        match op {
            FabricOp::PutCanonical { key, value } => {
                if let Some(BodyState::Collab(_)) = self.bodies.get(key) {
                    // A collaborative Body cannot be silently flattened.
                    return Err(FabricError::TypeConflict);
                }
                self.bodies
                    .insert(key.clone(), BodyState::Atomic(value.clone()));
                Ok(())
            }
            FabricOp::Remove { key } => {
                self.bodies.remove(key);
                Ok(())
            }
            FabricOp::CreateBody { key } => {
                self.collab_doc(key, true)?;
                Ok(())
            }
            FabricOp::RegisterSet { key, path, value } => {
                let doc = self.doc_for(key, "reg", path)?;
                let body = doc.get_map(BODY_MAP);
                body.insert(&typed_key("reg", path), value.as_slice())
                    .map_err(Self::loro_err)
            }
            FabricOp::RegisterClear { key, path } => {
                let doc = self.doc_for(key, "reg", path)?;
                let body = doc.get_map(BODY_MAP);
                let k = typed_key("reg", path);
                if body.get(&k).is_some() {
                    body.delete(&k).map_err(Self::loro_err)?;
                }
                Ok(())
            }
            FabricOp::MapSet {
                key,
                path,
                entry,
                value,
            } => {
                let doc = self.doc_for(key, "map", path)?;
                let m = doc.get_map(typed_key("map", path).as_str());
                m.insert(entry, value.as_slice()).map_err(Self::loro_err)
            }
            FabricOp::MapRemove { key, path, entry } => {
                let doc = self.doc_for(key, "map", path)?;
                let m = doc.get_map(typed_key("map", path).as_str());
                if m.get(entry).is_some() {
                    m.delete(entry).map_err(Self::loro_err)?;
                }
                Ok(())
            }
            FabricOp::ListInsert {
                key,
                path,
                index,
                value,
            } => {
                let doc = self.doc_for(key, "list", path)?;
                let l = doc.get_movable_list(typed_key("list", path).as_str());
                let index = *index as usize;
                if index > l.len() {
                    return Err(FabricError::InvalidOp("list index out of bounds".into()));
                }
                // Fabric mints the stable element id and embeds it in the value,
                // so identity survives synchronization.
                let id: [u8; ELEMENT_ID_LEN] = mint_bytes();
                let mut blob = Vec::with_capacity(ELEMENT_ID_LEN + value.len());
                blob.extend_from_slice(&id);
                blob.extend_from_slice(value);
                l.insert(index, blob.as_slice()).map_err(Self::loro_err)
            }
            FabricOp::ListRemove { key, path, element } => {
                let doc = self.doc_for(key, "list", path)?;
                let l = doc.get_movable_list(typed_key("list", path).as_str());
                let Some((i, _, _)) = Self::list_entries(&l)
                    .into_iter()
                    .find(|(_, id, _)| id == element)
                else {
                    return Err(FabricError::InvalidOp("unknown list element".into()));
                };
                l.delete(i, 1).map_err(Self::loro_err)
            }
            FabricOp::ListMove {
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
                    return Err(FabricError::InvalidOp("unknown list element".into()));
                };
                let to = *index as usize;
                if to >= l.len() {
                    return Err(FabricError::InvalidOp("list index out of bounds".into()));
                }
                l.mov(from, to).map_err(Self::loro_err)
            }
            FabricOp::TextSplice {
                key,
                path,
                index,
                delete,
                insert,
            } => {
                let doc = self.doc_for(key, "text", path)?;
                let t = doc.get_text(typed_key("text", path).as_str());
                let len = t.to_string().chars().count();
                let index = *index as usize;
                let delete = *delete as usize;
                if index + delete > len {
                    return Err(FabricError::InvalidOp("text splice out of bounds".into()));
                }
                if delete > 0 {
                    t.delete(index, delete).map_err(Self::loro_err)?;
                }
                if !insert.is_empty() {
                    t.insert(index, insert).map_err(Self::loro_err)?;
                }
                Ok(())
            }
            FabricOp::SetAdd { key, path, value } => {
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
            FabricOp::SetRemove { key, path, value } => {
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
            FabricOp::CounterAdd { key, path, delta } => {
                let doc = self.doc_for(key, "cnt", path)?;
                let peer = doc.peer_id();
                let m = doc.get_map(typed_key("cnt", path).as_str());
                // PN-counter: each doc session sums into its own peer key;
                // concurrent increments land in disjoint keys and always sum.
                let me = peer.to_string();
                let current = crate::loro_ext::get_i64(&m, &me).unwrap_or(0);
                let next = current
                    .checked_add(*delta)
                    .ok_or_else(|| FabricError::InvalidOp("counter overflow".into()))?;
                m.insert(&me, next).map_err(Self::loro_err)
            }
        }
    }

    /// The key an operation touches.
    fn op_key(op: &FabricOp) -> &FabricKey {
        match op {
            FabricOp::PutCanonical { key, .. }
            | FabricOp::Remove { key }
            | FabricOp::CreateBody { key }
            | FabricOp::RegisterSet { key, .. }
            | FabricOp::RegisterClear { key, .. }
            | FabricOp::MapSet { key, .. }
            | FabricOp::MapRemove { key, .. }
            | FabricOp::ListInsert { key, .. }
            | FabricOp::ListRemove { key, .. }
            | FabricOp::ListMove { key, .. }
            | FabricOp::TextSplice { key, .. }
            | FabricOp::SetAdd { key, .. }
            | FabricOp::SetRemove { key, .. }
            | FabricOp::CounterAdd { key, .. } => key,
        }
    }
}

impl Default for CrdtFabric {
    fn default() -> Self {
        Self::new()
    }
}

impl Fabric for CrdtFabric {
    fn commit(
        &mut self,
        request: FabricTransactionRequest,
    ) -> Result<FabricCommitReceipt, FabricError> {
        // Batch atomicity, bounded. This used to back every touched Body up by
        // full export before applying — a complete-history snapshot per
        // ordinary edit, paid inside Fabric before anything was sealed, and no
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
        let touched: std::collections::BTreeSet<FabricKey> = request
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
        let prior: BTreeMap<FabricKey, Option<(BodyState, Option<loro::Frontiers>)>> = touched
            .iter()
            .map(|key| {
                let snapshot = self.bodies.get(key).map(|state| match state {
                    // An atomic Body's whole value *is* its state, and it is
                    // already bounded by the Body envelope.
                    BodyState::Atomic(bytes) => (BodyState::Atomic(bytes.clone()), None),
                    BodyState::Collab(doc) => {
                        (BodyState::Collab(doc.clone()), Some(doc.oplog_frontiers()))
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
            let mut unrestored = 0usize;
            for (key, snapshot) in prior {
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
                        unrestored += 1;
                    }
                }
            }
            if unrestored > 0 {
                return Err(FabricError::Durability(format!(
                    "rollback after failed apply did not restore {unrestored} bodies"
                )));
            }
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
        Ok(FabricCommitReceipt::new(
            self.causal_for(&touched),
            request.ops.len() as u32,
        ))
    }

    fn read(&self, key: &FabricKey) -> Option<Vec<u8>> {
        match self.bodies.get(key)? {
            BodyState::Atomic(bytes) => Some(bytes.clone()),
            BodyState::Collab(_) => None,
        }
    }

    fn read_collaborative(&self, key: &FabricKey) -> Result<CollaborativeView, ProjectionError> {
        let Some(BodyState::Collab(doc)) = self.bodies.get(key) else {
            return Err(ProjectionError::NotCollaborative);
        };
        // The projection walks the doc's ROOT value tree once: registers live
        // as `reg:<path>` keys in the `body` root map; every other typed path
        // is a name-identified root container `<tag>:<path>`.
        let mut view = CollaborativeView::default();
        let loro::LoroValue::Map(roots) = doc.get_deep_value() else {
            return Ok(view);
        };
        for (name, value) in roots.iter() {
            if name == BODY_MAP {
                let loro::LoroValue::Map(body) = value else {
                    continue;
                };
                for (k, v) in body.iter() {
                    if let (Some(path), loro::LoroValue::Binary(bytes)) =
                        (k.strip_prefix("reg:"), v)
                    {
                        view.registers.insert(path.to_string(), bytes.to_vec());
                    }
                }
                continue;
            }
            let Some((tag, path)) = name.split_once(':') else {
                return Err(ProjectionError::SchemaAhead {
                    tag: name.to_string(),
                });
            };
            if !is_implemented_type_tag(tag) {
                return Err(ProjectionError::SchemaAhead {
                    tag: tag.to_string(),
                });
            }
            let path = path.to_string();
            match (tag, value) {
                ("map", loro::LoroValue::Map(m)) => {
                    let mut entries = BTreeMap::new();
                    for (k, v) in m.iter() {
                        if let loro::LoroValue::Binary(bytes) = v {
                            entries.insert(k.clone(), bytes.to_vec());
                        }
                    }
                    view.maps.insert(path, entries);
                }
                ("list", loro::LoroValue::List(items)) => {
                    let mut elements = Vec::new();
                    for v in items.iter() {
                        let loro::LoroValue::Binary(bytes) = v else {
                            continue;
                        };
                        if bytes.len() < ELEMENT_ID_LEN {
                            continue;
                        }
                        elements.push(ListElement {
                            element: data_encoding::HEXLOWER.encode(&bytes[..ELEMENT_ID_LEN]),
                            value: bytes[ELEMENT_ID_LEN..].to_vec(),
                        });
                    }
                    view.lists.insert(path, elements);
                }
                ("text", loro::LoroValue::String(text)) => {
                    view.texts.insert(path, text.to_string());
                }
                ("set", loro::LoroValue::Map(m)) => {
                    let mut members: Vec<Vec<u8>> = m
                        .values()
                        .filter_map(|v| match v {
                            loro::LoroValue::Binary(bytes) => Some(bytes.to_vec()),
                            _ => None,
                        })
                        .collect();
                    members.sort();
                    members.dedup();
                    view.sets.insert(path, members);
                }
                ("cnt", loro::LoroValue::Map(m)) => {
                    let total = m
                        .values()
                        .filter_map(|v| match v {
                            loro::LoroValue::I64(n) => Some(*n),
                            _ => None,
                        })
                        .fold(0i64, i64::saturating_add);
                    view.counters.insert(path, total);
                }
                _ => {
                    return Err(ProjectionError::Malformed {
                        tag: tag.to_string(),
                    })
                }
            }
        }
        Ok(view)
    }

    fn version(
        &self,
        key: &FabricKey,
    ) -> Result<crate::causal::FabricVersion, crate::causal::CausalError> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => Ok(crate::causal::FabricVersion::from_frontiers(
                &doc.oplog_frontiers(),
            )),
            // An atomic Body has one writer at a time and no operation history,
            // so its position is the empty one. It still answers, because a
            // caller should not have to know which model a key holds to ask.
            Some(BodyState::Atomic(_)) => Ok(crate::causal::FabricVersion::empty()),
            None => Err(crate::causal::CausalError::NotCollaborative),
        }
    }

    fn export_delta(
        &self,
        key: &FabricKey,
        from: &crate::causal::FabricVersion,
    ) -> Result<crate::causal::FabricArtifact, crate::causal::CausalError> {
        from.validate()?;
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => {
                let base = from.to_frontiers();
                let Some(vv) = doc.frontiers_to_vv(&base) else {
                    return Err(crate::causal::CausalError::MissingBase);
                };
                let bytes = doc
                    .export(loro::ExportMode::updates(&vv))
                    .map_err(|e| crate::causal::CausalError::Engine(e.to_string()))?;
                Ok(crate::causal::FabricArtifact::Delta {
                    format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                    base: from.clone(),
                    result: crate::causal::FabricVersion::from_frontiers(&doc.oplog_frontiers()),
                    bytes,
                })
            }
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::FabricArtifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.clone(),
            }),
            None => Err(crate::causal::CausalError::NotCollaborative),
        }
    }

    fn export_checkpoint(
        &self,
        key: &FabricKey,
        retention_frontier: &crate::causal::FabricVersion,
    ) -> Result<crate::causal::FabricArtifact, crate::causal::CausalError> {
        retention_frontier.validate()?;
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => {
                let frontier = if retention_frontier.is_empty() {
                    doc.oplog_frontiers()
                } else {
                    retention_frontier.to_frontiers()
                };
                if doc.frontiers_to_vv(&frontier).is_none() {
                    return Err(crate::causal::CausalError::MissingBase);
                }
                let bytes = doc
                    .export(loro::ExportMode::shallow_snapshot(&frontier))
                    .map_err(|e| crate::causal::CausalError::Engine(e.to_string()))?;
                Ok(crate::causal::FabricArtifact::Checkpoint {
                    format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                    retention_frontier: crate::causal::FabricVersion::from_frontiers(&frontier),
                    result: crate::causal::FabricVersion::from_frontiers(&doc.oplog_frontiers()),
                    bytes,
                })
            }
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::FabricArtifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.clone(),
            }),
            None => Err(crate::causal::CausalError::NotCollaborative),
        }
    }

    fn export_history(
        &self,
        key: &FabricKey,
    ) -> Result<crate::causal::FabricArtifact, crate::causal::CausalError> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => {
                let bytes = doc
                    .export(loro::ExportMode::Snapshot)
                    .map_err(|e| crate::causal::CausalError::Engine(e.to_string()))?;
                Ok(crate::causal::FabricArtifact::Archive {
                    format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                    result: crate::causal::FabricVersion::from_frontiers(&doc.oplog_frontiers()),
                    bytes,
                })
            }
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::FabricArtifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.clone(),
            }),
            None => Err(crate::causal::CausalError::NotCollaborative),
        }
    }

    fn import_artifact(
        &mut self,
        key: &FabricKey,
        artifact: &crate::causal::FabricArtifact,
    ) -> Result<crate::causal::ImportStatus, crate::causal::CausalError> {
        use crate::causal::{CausalError, FabricArtifact, FabricVersion, ImportStatus};

        if let FabricArtifact::Replace { bytes, .. } = artifact {
            let changed = !matches!(
                self.bodies.get(key),
                Some(BodyState::Atomic(current)) if current == bytes
            );
            if changed {
                self.bodies
                    .insert(key.clone(), BodyState::Atomic(bytes.clone()));
            }
            return Ok(ImportStatus {
                applied: changed,
                pending: false,
            });
        }

        let bytes = match artifact {
            FabricArtifact::Delta { bytes, .. }
            | FabricArtifact::Checkpoint { bytes, .. }
            | FabricArtifact::Archive { bytes, .. } => bytes,
            FabricArtifact::Replace { .. } => unreachable!("handled above"),
        };

        let writer = self.writer;
        let entry = self
            .bodies
            .entry(key.clone())
            .or_insert_with(|| BodyState::Collab(new_body_doc(Some(writer))));
        let BodyState::Collab(doc) = entry else {
            return Err(CausalError::NotCollaborative);
        };
        let before = doc.oplog_frontiers();
        let status = doc.import(bytes).map_err(|e| {
            // A compacted document refuses work whose dependencies precede its
            // shallow root. That refusal is the whole of §5.2 outcome 2, and it
            // is named rather than reported as a generic engine error so a
            // writer can act on it: rebuild from the archive, or re-bootstrap.
            let message = e.to_string();
            if message.contains("shallow") || message.contains("Shallow") {
                CausalError::BeforeRetentionFrontier {
                    frontier: FabricVersion::from_frontiers(&doc.shallow_since_frontiers()),
                }
            } else {
                CausalError::Engine(message)
            }
        })?;
        let after = doc.oplog_frontiers();
        Ok(ImportStatus {
            applied: before != after,
            pending: status.pending.is_some(),
        })
    }

    fn relation(
        &self,
        key: &FabricKey,
        a: &crate::causal::FabricVersion,
        b: &crate::causal::FabricVersion,
    ) -> crate::causal::CausalRelation {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => crate::causal::relation(doc, a, b),
            _ => crate::causal::CausalRelation::Undetermined,
        }
    }

    fn anchor(
        &self,
        key: &FabricKey,
        path: &str,
        position: u64,
    ) -> Result<crate::causal::FabricAnchor, crate::causal::CausalError> {
        let Some(BodyState::Collab(doc)) = self.bodies.get(key) else {
            return Err(crate::causal::CausalError::NotCollaborative);
        };
        let text = doc.get_text(typed_key("text", path));
        let length = text.len_unicode() as u64;
        // Bind the position to the operation that wrote the character before
        // it. That is what survives concurrent edits: an offset does not, and a
        // caret that does not survive a concurrent edit is worse than no caret.
        let anchored_to = if position == 0 || length == 0 {
            None
        } else {
            text.get_cursor(
                (position.min(length) as usize).saturating_sub(1),
                loro::cursor::Side::Right,
            )
            .and_then(|cursor| cursor.id)
            .map(|id| crate::causal::OpHead {
                writer: id.peer,
                sequence: id.counter,
            })
        };
        Ok(crate::causal::FabricAnchor {
            format_version: crate::causal::CAUSAL_FORMAT_VERSION,
            path: path.to_string(),
            anchored_to,
            offset: position,
            after: true,
            taken_at: crate::causal::FabricVersion::from_frontiers(&doc.oplog_frontiers()),
        })
    }

    fn resolve(
        &self,
        key: &FabricKey,
        anchor: &crate::causal::FabricAnchor,
    ) -> crate::causal::AnchorResolution {
        use crate::causal::AnchorResolution;
        let Some(BodyState::Collab(doc)) = self.bodies.get(key) else {
            return AnchorResolution::Drifted;
        };
        let text = doc.get_text(typed_key("text", &anchor.path));
        let length = text.len_unicode() as u64;

        // An anchor at the very start is the one position concurrent edits
        // cannot move.
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
            anchor.offset as usize,
        );
        match doc.get_cursor_pos(&cursor) {
            Ok(position) => {
                AnchorResolution::Resolved((position.current.pos as u64 + 1).min(length))
            }
            // Deleted material, or an anchor older than what this replica
            // retains. Either way the position cannot be mapped, and saying so
            // is the contract.
            Err(_) => AnchorResolution::Drifted,
        }
    }

    fn export_body(&self, key: &FabricKey) -> Option<BodyExport> {
        self.bodies.get(key).and_then(|s| s.export().ok())
    }

    fn import_body(
        &mut self,
        key: &FabricKey,
        export: &BodyExport,
    ) -> Result<Option<FabricCommitReceipt>, FabricError> {
        let changed = match (self.bodies.get(key), export) {
            // Atomic replacement — policy for concurrent atomic writes is
            // Replica's, decided before this call.
            (Some(BodyState::Atomic(current)), BodyExport::Atomic(bytes)) => {
                if current == bytes {
                    false
                } else {
                    self.bodies
                        .insert(key.clone(), BodyState::Atomic(bytes.clone()));
                    true
                }
            }
            (None, BodyExport::Atomic(bytes)) => {
                self.bodies
                    .insert(key.clone(), BodyState::Atomic(bytes.clone()));
                true
            }
            // Collaborative causal merge: already-known material is unchanged.
            (Some(BodyState::Collab(doc)), BodyExport::Collaborative(snapshot)) => {
                let before = doc.oplog_frontiers().encode();
                doc.import(snapshot)
                    .map_err(|e| FabricError::InvalidOp(format!("merge import: {e}")))?;
                doc.oplog_frontiers().encode() != before
            }
            (None, BodyExport::Collaborative(_)) => {
                self.bodies
                    .insert(key.clone(), BodyState::from_export(export)?);
                true
            }
            // A model mismatch at the same key is a type conflict, refused.
            (Some(BodyState::Atomic(_)), BodyExport::Collaborative(_))
            | (Some(BodyState::Collab(_)), BodyExport::Atomic(_)) => {
                return Err(FabricError::TypeConflict)
            }
        };
        if !changed {
            return Ok(None);
        }
        let mut touched = std::collections::BTreeSet::new();
        touched.insert(key.clone());
        Ok(Some(FabricCommitReceipt::new(self.causal_for(&touched), 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_export_carries_exactly_one_body() {
        // Two collaborative Bodies; exporting one and importing it elsewhere
        // must bring that Body only — never a cross-Body snapshot.
        let mut a = CrdtFabric::new();
        let k1 = FabricKey::from_bytes(b"body/1".to_vec());
        let k2 = FabricKey::from_bytes(b"body/2".to_vec());
        a.commit(FabricTransactionRequest::new(
            "created",
            vec![
                FabricOp::RegisterSet {
                    key: k1.clone(),
                    path: "title".into(),
                    value: b"one".to_vec(),
                },
                FabricOp::RegisterSet {
                    key: k2.clone(),
                    path: "title".into(),
                    value: b"two".to_vec(),
                },
            ],
        ))
        .unwrap();

        let export = a.export_body(&k1).unwrap();
        assert!(matches!(export, BodyExport::Collaborative(_)));
        let mut b = CrdtFabric::new();
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
        let mut a = CrdtFabric::new();
        let k = FabricKey::from_bytes(b"body/ids".to_vec());
        a.commit(FabricTransactionRequest::new(
            "created",
            vec![FabricOp::ListInsert {
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
        let mut b = CrdtFabric::new();
        b.import_body(&k, &a.export_body(&k).unwrap()).unwrap();
        b.commit(FabricTransactionRequest::new(
            "removed",
            vec![FabricOp::ListRemove {
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
        let mut a = CrdtFabric::new();
        let k = FabricKey::from_bytes(b"body/known".to_vec());
        a.commit(FabricTransactionRequest::new(
            "created",
            vec![FabricOp::CounterAdd {
                key: k.clone(),
                path: "votes".into(),
                delta: 2,
            }],
        ))
        .unwrap();
        let export = a.export_body(&k).unwrap();
        let mut b = CrdtFabric::new();
        assert!(b.import_body(&k, &export).unwrap().is_some(), "new");
        assert!(
            b.import_body(&k, &export).unwrap().is_none(),
            "already known — no receipt, nothing changed"
        );
        // Atomic idempotence too.
        let ak = FabricKey::from_bytes(b"body/atomic".to_vec());
        let atomic = BodyExport::Atomic(b"v1".to_vec());
        assert!(b.import_body(&ak, &atomic).unwrap().is_some());
        assert!(b.import_body(&ak, &atomic).unwrap().is_none());
    }

    #[test]
    fn a_model_mismatch_at_the_same_key_is_a_type_conflict() {
        let mut f = CrdtFabric::new();
        let k = FabricKey::from_bytes(b"body/mismatch".to_vec());
        f.commit(FabricTransactionRequest::new(
            "created",
            vec![FabricOp::PutCanonical {
                key: k.clone(),
                value: b"atomic".to_vec(),
            }],
        ))
        .unwrap();
        // A collaborative export addressed at an atomic key is refused.
        let mut other = CrdtFabric::new();
        other
            .commit(FabricTransactionRequest::new(
                "created",
                vec![FabricOp::CounterAdd {
                    key: k.clone(),
                    path: "votes".into(),
                    delta: 1,
                }],
            ))
            .unwrap();
        let collab = other.export_body(&k).unwrap();
        assert_eq!(
            f.import_body(&k, &collab).unwrap_err(),
            FabricError::TypeConflict
        );
        assert_eq!(f.read(&k).as_deref(), Some(&b"atomic"[..]), "unchanged");
    }

    #[test]
    fn transaction_request_roundtrips_postcard() {
        let req = FabricTransactionRequest::new(
            "created",
            vec![
                FabricOp::PutCanonical {
                    key: FabricKey::from_bytes(vec![1, 2, 3]),
                    value: vec![9],
                },
                FabricOp::Remove {
                    key: FabricKey::from_bytes(vec![4]),
                },
            ],
        );
        let bytes = postcard::to_stdvec(&req).unwrap();
        let back: FabricTransactionRequest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn atomic_bodies_commit_read_remove_and_advance_the_causal_token() {
        let mut fabric = CrdtFabric::new();
        let key = FabricKey::from_bytes(b"body/0".to_vec());
        let r1 = fabric
            .commit(FabricTransactionRequest::new(
                "created",
                vec![FabricOp::PutCanonical {
                    key: key.clone(),
                    value: b"v1".to_vec(),
                }],
            ))
            .unwrap();
        assert_eq!(r1.applied(), 1);
        assert_eq!(fabric.read(&key).as_deref(), Some(&b"v1"[..]));

        let r2 = fabric
            .commit(FabricTransactionRequest::new(
                "removed",
                vec![FabricOp::Remove { key: key.clone() }],
            ))
            .unwrap();
        // The causal token advances between commits.
        assert_ne!(r1.causal(), r2.causal());
        assert_eq!(fabric.read(&key), None);
    }
}
