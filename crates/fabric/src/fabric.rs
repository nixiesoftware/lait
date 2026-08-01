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
//! element identity) over real Loro containers — one Loro document per Body,
//! so per-Body export/import carries exactly one Body's causal history — with
//! batch atomicity. The durable store is the journaled protocol in
//! [`crate::journal`], persisting per-Body protected objects.

use std::collections::BTreeMap;

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
}

/// One ordered-list element: its stable Engine-minted id and its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListElement {
    pub element: String,
    pub value: Vec<u8>,
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

/// The Loro-backed engine: the canonical collaborative representation, and the
/// reason this crate alone names Loro.
///
/// **Layout — one Loro document per Body.** Each collaborative Body is its own
/// `LoroDoc`, so its canonical export ([`fabric::export_body`]) carries exactly
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
pub struct Engine {
    /// This activation's writer id, minted once and shared by every document
    /// this engine authors into. One id per activation rather than one per
    /// document keeps a Body's version vector growing with restarts rather than
    /// with Bodies, and never persisting it is what keeps a copied store from
    /// minting colliding operation ids.
    writer: u64,
    bodies: BTreeMap<Key, BodyState>,
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

impl BodyState {
    /// A position digest for the causal token: the atomic bytes' hash, or the
    /// collaborative doc's oplog frontier.
    fn digest(&self) -> Vec<u8> {
        match self {
            BodyState::Atomic(bytes) => blake3::hash(bytes).as_bytes().to_vec(),
            BodyState::Collab(doc) => doc.oplog_frontiers().encode().to_vec(),
        }
    }

    fn export(&self) -> Result<BodyExport, Failure> {
        match self {
            BodyState::Atomic(bytes) => Ok(BodyExport::Atomic(bytes.clone())),
            BodyState::Collab(doc) => doc
                .export(loro::ExportMode::Snapshot)
                .map(BodyExport::Collaborative)
                .map_err(|_| Failure::Invalid(commit::Invalid::Import)),
        }
    }

    fn from_export(export: &BodyExport) -> Result<Self, Failure> {
        match export {
            BodyExport::Atomic(bytes) => Ok(BodyState::Atomic(bytes.clone())),
            BodyExport::Collaborative(snapshot) => {
                // A doc reconstructed from an export is not yet authoring, so
                // it keeps Loro's own id until this activation writes into it
                // through `collab_doc`.
                let doc = new_body_doc(None);
                doc.import(snapshot)
                    .map_err(|_| Failure::Invalid(commit::Invalid::Import))?;
                Ok(BodyState::Collab(doc))
            }
        }
    }
}

impl Engine {
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
    pub fn body_keys(&self) -> Vec<Key> {
        self.bodies.keys().cloned().collect()
    }

    fn loro_err(e: impl std::fmt::Display) -> Failure {
        tracing::warn!(%e, "fabric operation was invalid");
        Failure::Invalid(commit::Invalid::Import)
    }

    /// The collaborative doc for a Body, creating it when `create`. An atomic
    /// value at the key is a [`Failure::TypeConflict`].
    fn collab_doc(&mut self, key: &Key, create: bool) -> Result<Option<&loro::LoroDoc>, Failure> {
        use std::collections::btree_map::Entry;
        match self.bodies.entry(key.clone()) {
            Entry::Occupied(e) => match e.into_mut() {
                BodyState::Collab(doc) => Ok(Some(doc)),
                BodyState::Atomic(_) => Err(Failure::TypeConflict),
            },
            Entry::Vacant(v) if create => {
                match v.insert(BodyState::Collab(new_body_doc(Some(self.writer)))) {
                    BodyState::Collab(doc) => Ok(Some(doc)),
                    BodyState::Atomic(_) => Err(Failure::TypeConflict),
                }
            }
            Entry::Vacant(_) => Ok(None),
        }
    }

    /// Enforce "a path is bound to exactly one collaborative type": no other
    /// type tag may already hold state at this path. Containers live at doc
    /// ROOTS (name-identified — see the struct docs); a register is a key in
    /// the `body` root map.
    fn check_path_type(doc: &loro::LoroDoc, tag: &str, path: &str) -> Result<(), Failure> {
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

    /// The causal token digesting the touched Bodies' post-commit positions.
    fn causal_for(&self, touched: &std::collections::BTreeSet<Key>) -> CausalToken {
        let mut h = blake3::Hasher::new();
        h.update(CAUSAL_DOMAIN);
        for key in touched {
            let key_len = u64::try_from(key.as_bytes().len()).unwrap_or(u64::MAX);
            h.update(&key_len.to_le_bytes());
            h.update(key.as_bytes());
            match self.bodies.get(key) {
                Some(state) => {
                    let digest = state.digest();
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
        CausalToken::from_bytes(h.finalize().as_bytes().to_vec())
    }

    /// Apply one operation. Errors leave partially-applied state in the touched
    /// Body; [`fabric::commit`] rolls the whole batch back from its backups.
    fn apply(&mut self, op: &Op) -> Result<(), Failure> {
        match op {
            Op::PutCanonical { key, value } => {
                if let Some(BodyState::Collab(_)) = self.bodies.get(key) {
                    // A collaborative Body cannot be silently flattened.
                    return Err(Failure::TypeConflict);
                }
                self.bodies
                    .insert(key.clone(), BodyState::Atomic(value.clone()));
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
            | Op::CounterAdd { key, .. } => key,
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn commit(&mut self, request: Transaction) -> Result<Receipt, Failure> {
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
                        unrestored = unrestored.saturating_add(1);
                    }
                }
            }
            if unrestored > 0 {
                tracing::error!(unrestored, "fabric rollback did not restore all bodies");
                return Err(Failure::OutcomeUnknown);
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
        let applied = u32::try_from(request.ops.len())
            .map_err(|_| Failure::Invalid(commit::Invalid::Bounds))?;
        Ok(Receipt::new(self.causal_for(&touched), applied))
    }

    pub fn read(&self, key: &Key) -> Option<Vec<u8>> {
        match self.bodies.get(key)? {
            BodyState::Atomic(bytes) => Some(bytes.clone()),
            BodyState::Collab(_) => None,
        }
    }

    pub fn read_collaborative(&self, key: &Key) -> Result<CollaborativeView, ProjectionFailure> {
        let Some(BodyState::Collab(doc)) = self.bodies.get(key) else {
            return Err(ProjectionFailure::NotCollaborative);
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
                tracing::warn!(tag = %name, "unrecognized collaborative root");
                return Err(ProjectionFailure::SchemaAhead);
            };
            if !is_implemented_type_tag(tag) {
                tracing::warn!(tag, "unsupported collaborative type");
                return Err(ProjectionFailure::SchemaAhead);
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
                        let Some((identity, value)) = bytes.split_at_checked(ELEMENT_ID_LEN) else {
                            continue;
                        };
                        elements.push(ListElement {
                            element: data_encoding::HEXLOWER.encode(identity),
                            value: value.to_vec(),
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
                    tracing::warn!(tag, "collaborative type held an invalid value");
                    return Err(ProjectionFailure::Malformed);
                }
            }
        }
        Ok(view)
    }

    pub fn version(&self, key: &Key) -> Result<crate::causal::Version, crate::causal::Invalid> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => Ok(crate::causal::Version::from_frontiers(
                &doc.oplog_frontiers(),
            )),
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
            Some(BodyState::Collab(doc)) => {
                let base = from.to_frontiers();
                let Some(vv) = doc.frontiers_to_vv(&base) else {
                    return Err(crate::causal::Invalid::MissingBase);
                };
                let bytes = doc
                    .export(loro::ExportMode::updates(&vv))
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
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::Artifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.clone(),
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
            Some(BodyState::Collab(doc)) => {
                let frontier = if retention_frontier.is_empty() {
                    doc.oplog_frontiers()
                } else {
                    retention_frontier.to_frontiers()
                };
                if doc.frontiers_to_vv(&frontier).is_none() {
                    return Err(crate::causal::Invalid::MissingBase);
                }
                let bytes = doc
                    .export(loro::ExportMode::shallow_snapshot(&frontier))
                    .map_err(|source| {
                        tracing::warn!(%source, "fabric checkpoint export failed");
                        crate::causal::Invalid::Engine
                    })?;
                Ok(crate::causal::Artifact::Checkpoint {
                    format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                    retention_frontier: crate::causal::Version::from_frontiers(&frontier),
                    result: crate::causal::Version::from_frontiers(&doc.oplog_frontiers()),
                    bytes,
                })
            }
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::Artifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.clone(),
            }),
            None => Err(crate::causal::Invalid::NotCollaborative),
        }
    }

    pub fn export_history(
        &self,
        key: &Key,
    ) -> Result<crate::causal::Artifact, crate::causal::Invalid> {
        match self.bodies.get(key) {
            Some(BodyState::Collab(doc)) => {
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
            Some(BodyState::Atomic(bytes)) => Ok(crate::causal::Artifact::Replace {
                format_version: crate::causal::CAUSAL_FORMAT_VERSION,
                bytes: bytes.clone(),
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
            if matches!(self.bodies.get(key), Some(BodyState::Collab(_))) {
                return Err(Invalid::NotCollaborative);
            }
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
            Artifact::Delta { bytes, .. }
            | Artifact::Checkpoint { bytes, .. }
            | Artifact::Archive { bytes, .. } => bytes,
            Artifact::Replace { .. } => return Err(Invalid::Engine),
        };

        let writer = self.writer;
        let entry = self
            .bodies
            .entry(key.clone())
            .or_insert_with(|| BodyState::Collab(new_body_doc(Some(writer))));
        let BodyState::Collab(doc) = entry else {
            return Err(Invalid::NotCollaborative);
        };
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
            _ => crate::causal::CausalRelation::Undetermined,
        }
    }

    pub fn anchor(
        &self,
        key: &Key,
        path: &str,
        position: u64,
    ) -> Result<crate::causal::Anchor, crate::causal::Invalid> {
        let Some(BodyState::Collab(doc)) = self.bodies.get(key) else {
            return Err(crate::causal::Invalid::NotCollaborative);
        };
        let text = doc.get_text(typed_key("text", path));
        let length = u64::try_from(text.len_unicode()).unwrap_or(u64::MAX);
        // Bind the position to the operation that wrote the character before
        // it. That is what survives concurrent edits: an offset does not, and a
        // caret that does not survive a concurrent edit is worse than no caret.
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

    pub fn resolve(
        &self,
        key: &Key,
        anchor: &crate::causal::Anchor,
    ) -> crate::causal::AnchorResolution {
        use crate::causal::AnchorResolution;
        if anchor.body != body_digest(key) {
            return AnchorResolution::Drifted;
        }
        let Some(BodyState::Collab(doc)) = self.bodies.get(key) else {
            return AnchorResolution::Drifted;
        };
        let text = doc.get_text(typed_key("text", &anchor.path));
        let length = u64::try_from(text.len_unicode()).unwrap_or(u64::MAX);

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
            usize::try_from(anchor.offset).unwrap_or(usize::MAX),
        );
        match doc.get_cursor_pos(&cursor) {
            // The anchor bound to the character *before* the caret, so a live
            // resolution sits one past it — and only a live one. When the
            // anchored character is gone the engine answers with the gap it
            // left, flipping the side; adding one to that puts the caret a
            // character to the right of where it belongs. `Drifted` is what
            // this case is documented to be, and it is what a renderer can act
            // on.
            Ok(position) if position.current.side == loro::cursor::Side::Right => {
                let resolved = u64::try_from(position.current.pos)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1)
                    .min(length);
                AnchorResolution::Resolved(resolved)
            }
            Ok(_) => AnchorResolution::Drifted,
            // An anchor older than what this replica retains.
            Err(_) => AnchorResolution::Drifted,
        }
    }

    pub fn export_body(&self, key: &Key) -> Option<BodyExport> {
        self.bodies.get(key).and_then(|s| s.export().ok())
    }

    pub fn import_body(
        &mut self,
        key: &Key,
        export: &BodyExport,
    ) -> Result<Option<Receipt>, Failure> {
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
                    .map_err(|_| Failure::Invalid(commit::Invalid::Merge))?;
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
                return Err(Failure::TypeConflict)
            }
        };
        if !changed {
            return Ok(None);
        }
        let mut touched = std::collections::BTreeSet::new();
        touched.insert(key.clone());
        Ok(Some(Receipt::new(self.causal_for(&touched), 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
