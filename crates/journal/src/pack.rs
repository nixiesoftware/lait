//! The pack log: the append-only physical format the store is moving to.
//!
//! One slot holds the live pack — a header, then OBJECT and SEAL records,
//! appended and never rewritten. A commit appends its objects and one seal,
//! then flushes **once**; everything up to the last verified seal is the
//! store's durable state. Where the prior format spends a fsync per phase
//! and stakes atomicity on rename — the two operations browsers cannot
//! promise and native filesystems keep only reluctantly — this one spends its
//! whole budget on a single flush and takes its atomicity from content.
//!
//! The design was validated against the crash-consistency literature and the
//! measured browser platform; the load-bearing conclusions:
//!
//! - **A structurally valid seal is not a valid commit.** Between flushes the
//!   storage stack persists blocks in any order (the ALICE append model), so
//!   a crash can keep a seal and lose object bytes before it. Recovery
//!   therefore re-hashes every object the candidate seal's delta names, and
//!   steps back one seal on failure. One flush per commit is what makes this
//!   sufficient: at most the newest commits are unflushed.
//! - **A slot's past is an adversary.** A stale tail can contain a genuinely
//!   checksummed seal from a prior life of the file, and an object payload
//!   can embed a genuine record of this life verbatim. Every record check is
//!   keyed by a per-slot random salt and bound to the record's offset, and
//!   every seal names its predecessor's offset and check, so nothing from
//!   another life, another slot, or another position can join the chain.
//! - **Generations are elected by sequence, not by generation.** Compaction
//!   copies live objects into the next slot and seals it one generation up.
//!   A crash can leave several slots; the one whose verified seal covers the
//!   highest sequence wins, generation only breaking the tie — and a birth
//!   seal (`prev: None`) verifies its whole checkpoint, not just a delta, so
//!   a torn successor loses the election instead of taking the intact
//!   predecessor down with it. (The price: the first open after a compaction
//!   or migration re-hashes the live set once, until the next commit gives
//!   the slot a delta seal.) Slot names are **monotonic**
//!   (`<prefix>-<generation>`) and never reused while any reader could hold
//!   them; the two reuse sites that exist — a compaction retry recreating
//!   its failed successor, and a stillborn migration recreating generation
//!   zero — both run with provably no reader alive. The header names its
//!   slot, so a medium that recycles physical files cannot pass one slot's
//!   history off as another's.
//! - **A failed flush poisons the writer.** After fsyncgate, a retried flush
//!   that "succeeds" proves nothing. The commit is [`Failure::OutcomeUnknown`]
//!   and so is every call after it; reopening — a fresh handle and a full
//!   verified recovery — is the only way back.
//!
//! ## Readers, snapshots, and the two invariants
//!
//! A [`PackView`] is a **snapshot**: the generation's shared read half plus
//! the table as of one seal. Views live on other threads, across commits,
//! across compactions. Two invariants keep every read sound with no lock on
//! the read path:
//!
//! - **I1 — addressability**: a view's table names only ranges below its
//!   `sealed_len`, the slot length its seal covered.
//! - **I2 — ordering**: the writer flushes appended bytes before publishing
//!   the snapshot that names them, and truncates only above every published
//!   snapshot's `sealed_len` (a failed commit's undo discards the unsealed
//!   tail alone).
//!
//! Together the ranges a view can read and the ranges the writer mutates are
//! disjoint. A compaction retires the old generation into a **graveyard**;
//! its slot is removed only when the last view drains — an act of the store
//! at the next compaction or open, never a side effect on a reader's thread.
//!
//! Open cost is O(seals since the last checkpoint), not O(pack): recovery
//! finds the newest seal by scanning back from the end, then walks the seal
//! chain, and every [`CHECKPOINT_EVERY`]th seal carries the full table so the
//! walk is bounded. Reads verify content addresses, as everywhere else.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::medium::{Medium, ReadAt, SlotWriter};
use crate::{object_content_hash, Defect, Failure, IoKind, Operation};

const SLOT_MAGIC: &[u8; 8] = b"laitpak2";
const SALT_LEN: usize = 16;
/// magic 8 | salt 16 | name_len u16 | name bytes, zero-padded to 64. The
/// header names its own slot so bytes that reappear under the wrong name — a
/// recycled physical file on a medium that pools them — are rejected
/// structurally, not probabilistically: a resurrected slot is self-consistent
/// under its own salt, and only the recorded name says it is history.
const HEADER_LEN: u64 = 64;
const HEADER_NAME_CAPACITY: usize = 38;
const OBJECT_MAGIC: [u8; 4] = *b"lpo1";
const SEAL_MAGIC: [u8; 4] = *b"lps1";
const CHECK_LEN: usize = 16;
/// magic 4 + len 4 + trailing check 16.
const RECORD_OVERHEAD: u64 = 24;
/// Refuses garbage length prefixes long before an allocation is attempted.
const MAX_RECORD_BYTES: u32 = 256 * 1024 * 1024;
/// A full-table checkpoint seal at least this often bounds the recovery walk.
const CHECKPOINT_EVERY: u64 = 64;
/// How far back the newest-seal scan reads per step.
const SCAN_WINDOW: u64 = 64 * 1024;
const RECORD_KEY_CONTEXT: &str = "lait/pack-record/1";

/// Every named crash boundary in the pack log, for fault-matrix coverage.
/// `pack-compact-retire` is post-authoritative: a crash there loses cleanup,
/// never a commit.
#[cfg(any(test, feature = "fault-injection"))]
pub const PACK_FAULT_POINTS: &[&str] = &[
    "pack-objects",
    "pack-seal",
    "pack-flush",
    "pack-compact-objects",
    "pack-compact-seal",
    "pack-compact-flush",
    "pack-compact-retire",
];

/// Where a pack came from, when it came from somewhere: the first seal of a
/// migrated store records what it consumed, so a lingering source that was
/// mutated afterwards is a detectable divergence, never a silent retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Hash of the source's manifest bytes at migration time.
    pub source_manifest: [u8; 32],
    /// The source's reserved-sequence high water; this pack's sequences
    /// continue strictly above it.
    pub source_counter: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct TableEntry {
    hash: [u8; 32],
    offset: u64,
    len: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct SealLink {
    offset: u64,
    check: [u8; CHECK_LEN],
}

#[derive(Debug, Serialize, Deserialize)]
struct Seal {
    sequence: u64,
    generation: u64,
    prev: Option<SealLink>,
    added: Vec<TableEntry>,
    /// The complete table, carried by checkpoint seals; recovery stops its
    /// backward walk here.
    checkpoint: Option<Vec<TableEntry>>,
    provenance: Option<Provenance>,
    manifest: Vec<u8>,
}

type Table = imbl::OrdMap<[u8; 32], (u64, u64)>;

/// One slot life: its name and its shared read half. Retired generations
/// stay readable through the views that pinned them.
struct Generation {
    name: String,
    read: Arc<dyn ReadAt>,
}

/// The published state one commit exposed: a generation, its table, and the
/// sealed length that bounds every range the table names (invariant I1).
struct Snapshot {
    generation: Arc<Generation>,
    table: Table,
    sealed_len: u64,
    sequence: u64,
}

/// A read-only view of the pack at one seal. Cloneable, thread-safe, valid
/// for as long as it is held — a compaction retires its generation but never
/// its bytes. Reads verify content addresses.
#[derive(Clone)]
pub struct PackView {
    snapshot: Arc<Snapshot>,
}

impl std::fmt::Debug for PackView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackView")
            .field("generation", &self.snapshot.generation.name)
            .field("sequence", &self.snapshot.sequence)
            .field("objects", &self.snapshot.table.len())
            .finish()
    }
}

impl PackView {
    #[must_use]
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.snapshot.table.contains_key(hash)
    }

    /// How many objects this view's seal names — required or not, index
    /// nodes included. The physical population, where the required set is
    /// the semantic one.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.snapshot.table.len()
    }

    /// The length the table records for one object, if it is present.
    #[must_use]
    pub fn object_len(&self, hash: &[u8; 32]) -> Option<u64> {
        self.snapshot.table.get(hash).map(|(_, len)| *len)
    }

    /// Read one object, verified against its content address.
    pub fn read(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Failure> {
        self.read_bounded(hash, u64::MAX)
    }

    /// Read one object of at most `max_len` bytes; the bound is checked
    /// against the table before anything is allocated or read.
    pub fn read_bounded(&self, hash: &[u8; 32], max_len: u64) -> Result<Vec<u8>, Failure> {
        let (offset, len) = self
            .snapshot
            .table
            .get(hash)
            .copied()
            .ok_or(Failure::Integrity(Defect::MissingObject))?;
        if len > max_len {
            return Err(Failure::Integrity(Defect::CorruptObject));
        }
        debug_assert!(
            offset.saturating_add(len) <= self.snapshot.sealed_len,
            "a view's table must never name bytes past its seal"
        );
        let capacity = usize::try_from(len).map_err(|_| corrupt())?;
        let mut bytes = vec![0u8; capacity];
        self.snapshot
            .generation
            .read
            .read_at(offset, &mut bytes)
            .map_err(|e| io_err(Operation::Read, &e))?;
        if object_content_hash(&bytes) != *hash {
            return Err(Failure::Integrity(Defect::CorruptObject));
        }
        Ok(bytes)
    }
}

/// The pack-log store engine: semantics-free, single-writer, one flush per
/// commit. `prefix` names the slot family (`<prefix>-<generation>`) so one
/// medium can carry several packs — a hot log and a cold one for large
/// payloads, whose compaction schedules must differ.
pub struct PackStore {
    medium: Arc<dyn Medium>,
    writer: Box<dyn SlotWriter>,
    prefix: String,
    key: [u8; 32],
    snapshot: Arc<Snapshot>,
    graveyard: Vec<Arc<Generation>>,
    generation: u64,
    last_seal: Option<SealLink>,
    seals_since_checkpoint: u64,
    provenance: Option<Provenance>,
    manifest: Option<Vec<u8>>,
    poisoned: bool,
    injector: Option<crate::FaultInjector>,
}

impl std::fmt::Debug for PackStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackStore")
            .field("slot", &self.snapshot.generation.name)
            .field("sequence", &self.snapshot.sequence)
            .field("generation", &self.generation)
            .field("objects", &self.snapshot.table.len())
            .finish_non_exhaustive()
    }
}

fn io_err(operation: Operation, error: &std::io::Error) -> Failure {
    tracing::warn!(%error, ?operation, "pack operation failed");
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => IoKind::NotFound,
        std::io::ErrorKind::PermissionDenied => IoKind::PermissionDenied,
        std::io::ErrorKind::Interrupted => IoKind::Interrupted,
        std::io::ErrorKind::InvalidData => IoKind::InvalidData,
        _ => IoKind::Other,
    };
    Failure::Operation { operation, kind }
}

fn corrupt() -> Failure {
    Failure::Integrity(Defect::CorruptJournal)
}

fn encode_failure() -> Failure {
    Failure::Operation {
        operation: Operation::Encode,
        kind: IoKind::InvalidData,
    }
}

/// The check binds the record's **offset** as well as its bytes: a genuine
/// record embedded verbatim inside another record's payload (a pack backup
/// stored as an object, say) shares the salt, and only its position tells the
/// tail scan it is data, not a record.
fn record_check(key: &[u8; 32], magic: [u8; 4], offset: u64, payload: &[u8]) -> [u8; CHECK_LEN] {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(&offset.to_le_bytes());
    hasher.update(&magic);
    hasher.update(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    hasher.update(payload);
    let mut check = [0u8; CHECK_LEN];
    if let Some(head) = hasher.finalize().as_bytes().get(..CHECK_LEN) {
        check.copy_from_slice(head);
    }
    check
}

/// What recovery concluded about one slot. `None` means no commit survives
/// there — resettable, never trusted.
struct SlotState {
    len: u64,
    table: Table,
    sequence: u64,
    generation: u64,
    last_seal: SealLink,
    seals_since_checkpoint: u64,
    provenance: Option<Provenance>,
    manifest: Vec<u8>,
}

struct RecoveredSlot {
    name: String,
    writer: Box<dyn SlotWriter>,
    read: Arc<dyn ReadAt>,
    key: [u8; 32],
    state: Option<SlotState>,
    /// A birth seal was found whose checkpoint failed verification: the slot
    /// holds acknowledged history that cannot be exposed. Such a slot may
    /// lose an election to a verified sibling, but where nothing verifies it
    /// must fail the open — founding a fresh pack over it would erase the
    /// store, and deleting it would erase what a restore could still want.
    torn_birth: bool,
}

impl PackStore {
    /// Open a pack family, electing the slot whose verified seal covers the
    /// highest sequence (generation breaks ties), removing the losers, and
    /// truncating the winner's unverified tail.
    pub fn open(medium: Arc<dyn Medium>, prefix: &str) -> Result<Self, Failure> {
        let names = slot_family(medium.as_ref(), prefix)?;
        let mut recovered: Vec<RecoveredSlot> = Vec::new();
        for name in names {
            let (writer, read) = medium
                .open_slot(&name)
                .map_err(|e| io_err(Operation::Open, &e))?;
            recovered.push(recover_slot(name, writer, read)?);
        }
        // A torn birth seal is acknowledged history nothing can expose. With
        // a verified sibling it simply loses the election; alone, the only
        // honest outcomes are refusal here or erasure below — and erasure is
        // never this code's call.
        if recovered.iter().all(|r| r.state.is_none()) && recovered.iter().any(|r| r.torn_birth) {
            tracing::warn!(
                prefix,
                "the pack's only history is a torn birth seal; refusing to reset"
            );
            return Err(Failure::Integrity(Defect::CorruptObject));
        }
        recovered.sort_by_key(|r| {
            r.state
                .as_ref()
                .map_or((0, 0), |s| (s.sequence, s.generation))
        });
        let mut elected = None;
        if let Some(last) = recovered.pop() {
            if last.state.is_some() {
                elected = Some(last);
            } else {
                recovered.push(last);
            }
        }
        // Losers and unusable slots are retired: their handles first, then
        // their names. A failed removal costs a retry at the next open.
        for loser in recovered {
            drop(loser.writer);
            drop(loser.read);
            let _ = medium.remove_slot(&loser.name);
        }
        let Some(RecoveredSlot {
            name,
            mut writer,
            read,
            key,
            state: Some(state),
            ..
        }) = elected
        else {
            // No slot holds a verified seal: initialize a fresh pack at
            // generation 0. Any unverified bytes (a crash during first init)
            // were discarded with their slot above.
            let name = slot_name(prefix, 0);
            let (writer, read, key) = init_slot(medium.as_ref(), &name)?;
            let sealed_len = writer.len();
            let snapshot = Arc::new(Snapshot {
                generation: Arc::new(Generation { name, read }),
                table: Table::new(),
                sealed_len,
                sequence: 0,
            });
            return Ok(Self {
                medium,
                writer,
                prefix: prefix.to_owned(),
                key,
                snapshot,
                graveyard: Vec::new(),
                generation: 0,
                last_seal: None,
                seals_since_checkpoint: 0,
                provenance: None,
                manifest: None,
                poisoned: false,
                injector: None,
            });
        };
        if writer.len() > state.len {
            writer
                .truncate(state.len)
                .map_err(|e| io_err(Operation::Open, &e))?;
            // Without this flush the truncate could reorder against the next
            // commit's appends and leave the old tail alive behind them.
            writer.flush().map_err(|e| io_err(Operation::Sync, &e))?;
        }
        let snapshot = Arc::new(Snapshot {
            generation: Arc::new(Generation { name, read }),
            table: state.table,
            sealed_len: state.len,
            sequence: state.sequence,
        });
        Ok(Self {
            medium,
            writer,
            prefix: prefix.to_owned(),
            key,
            snapshot,
            graveyard: Vec::new(),
            generation: state.generation,
            last_seal: Some(state.last_seal),
            seals_since_checkpoint: state.seals_since_checkpoint,
            provenance: state.provenance,
            manifest: Some(state.manifest),
            poisoned: false,
            injector: None,
        })
    }

    /// Attach a fault injector (test seam; see [`PACK_FAULT_POINTS`]).
    #[must_use]
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn with_fault_injector(mut self, injector: Box<dyn Fn(&str) -> bool + Send>) -> Self {
        self.injector = Some(injector);
        self
    }

    /// The by-reference form, for a store embedding this pack behind a lock.
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn set_fault_injector(&mut self, injector: Box<dyn Fn(&str) -> bool + Send>) {
        self.injector = Some(injector);
    }

    /// The manifest bytes sealed by the last commit, `None` for a fresh pack.
    #[must_use]
    pub fn manifest(&self) -> Option<&[u8]> {
        self.manifest.as_deref()
    }

    /// The last committed sequence; 0 for a fresh pack.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.snapshot.sequence
    }

    /// The provenance the elected seal carries, when this pack was born by
    /// migration and nothing has been committed since.
    #[must_use]
    pub fn provenance(&self) -> Option<&Provenance> {
        self.provenance.as_ref()
    }

    #[must_use]
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.snapshot.table.contains_key(hash)
    }

    /// A snapshot of the pack at its last seal, for readers on any thread.
    #[must_use]
    pub fn view(&self) -> PackView {
        PackView {
            snapshot: self.snapshot.clone(),
        }
    }

    /// Read one object, verified against its content address.
    pub fn read(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Failure> {
        self.view().read(hash)
    }

    /// Append `objects` and one seal carrying `manifest`, then flush once.
    /// Returns the committed sequence. A flush failure is
    /// [`Failure::OutcomeUnknown`] and poisons the writer: every later call
    /// answers the same until the pack is reopened.
    pub fn commit(&mut self, objects: &[Vec<u8>], manifest: Vec<u8>) -> Result<u64, Failure> {
        self.commit_inner(objects, manifest)
    }

    /// The one commit a fresh pack may take from a migration: it streams the
    /// source's objects (so a store migrates in constant memory), seeds the
    /// sequence strictly above everything the source ever reserved, and
    /// seals the source's identity in as [`Provenance`]. The seal is a
    /// checkpoint, so recovery never walks past a store's birth.
    pub fn migrate_commit(
        &mut self,
        objects: &mut dyn Iterator<Item = Result<Vec<u8>, Failure>>,
        manifest: Vec<u8>,
        provenance: Provenance,
    ) -> Result<u64, Failure> {
        if self.poisoned {
            return Err(Failure::OutcomeUnknown);
        }
        if self.last_seal.is_some() {
            return Err(corrupt());
        }
        match self.try_migrate(objects, manifest, provenance) {
            Ok(sequence) => Ok(sequence),
            Err(Failure::OutcomeUnknown) => Err(Failure::OutcomeUnknown),
            Err(failure) => {
                if self.writer.truncate(HEADER_LEN).is_err() {
                    self.poisoned = true;
                }
                Err(failure)
            }
        }
    }

    fn try_migrate(
        &mut self,
        objects: &mut dyn Iterator<Item = Result<Vec<u8>, Failure>>,
        manifest: Vec<u8>,
        provenance: Provenance,
    ) -> Result<u64, Failure> {
        self.point("pack-objects")?;
        let mut table = Table::new();
        for bytes in objects {
            let bytes = bytes?;
            let hash = object_content_hash(&bytes);
            if table.contains_key(&hash) {
                continue;
            }
            let offset = self.append_record(OBJECT_MAGIC, &bytes)?;
            let len = u64::try_from(bytes.len()).map_err(|_| corrupt())?;
            table.insert(hash, (offset, len));
        }
        let sequence = provenance
            .source_counter
            .checked_add(1)
            .ok_or(Failure::Integrity(Defect::CounterOverflow))?;
        let seal = Seal {
            sequence,
            generation: self.generation,
            prev: None,
            added: Vec::new(),
            checkpoint: Some(entries(&table)),
            provenance: Some(provenance),
            manifest: manifest.clone(),
        };
        self.point("pack-seal")?;
        let link = self.append_seal(&seal)?;
        self.point("pack-flush")?;
        if let Err(error) = self.writer.flush() {
            tracing::warn!(%error, "pack flush failed; writer poisoned");
            self.poisoned = true;
            return Err(Failure::OutcomeUnknown);
        }
        self.snapshot = Arc::new(Snapshot {
            generation: self.snapshot.generation.clone(),
            table,
            sealed_len: self.writer.len(),
            sequence,
        });
        self.last_seal = Some(link);
        self.seals_since_checkpoint = 0;
        self.provenance = Some(provenance);
        self.manifest = Some(manifest);
        Ok(sequence)
    }

    fn commit_inner(&mut self, objects: &[Vec<u8>], manifest: Vec<u8>) -> Result<u64, Failure> {
        if self.poisoned {
            return Err(Failure::OutcomeUnknown);
        }
        let undo = self.writer.len();
        debug_assert!(
            undo >= self.snapshot.sealed_len,
            "the writer must never sit below the published seal"
        );
        match self.try_commit(objects, manifest) {
            Ok(sequence) => Ok(sequence),
            Err(Failure::OutcomeUnknown) => Err(Failure::OutcomeUnknown),
            Err(failure) => {
                // The seal never flushed, so nothing was committed; drop the
                // partial tail now if the medium will let us, or poison and
                // let recovery discard it the same way. The truncate needs no
                // flush of its own: the next commit flushes before claiming
                // anything, and until then nothing references the tail.
                if self.writer.truncate(undo).is_err() {
                    self.poisoned = true;
                }
                Err(failure)
            }
        }
    }

    fn try_commit(&mut self, objects: &[Vec<u8>], manifest: Vec<u8>) -> Result<u64, Failure> {
        self.point("pack-objects")?;
        let mut added = Vec::new();
        let mut table = self.snapshot.table.clone();
        for bytes in objects {
            let hash = object_content_hash(bytes);
            if table.contains_key(&hash) {
                continue;
            }
            let offset = self.append_record(OBJECT_MAGIC, bytes)?;
            let len = u64::try_from(bytes.len()).map_err(|_| corrupt())?;
            table.insert(hash, (offset, len));
            added.push(TableEntry { hash, offset, len });
        }
        let sequence = self
            .snapshot
            .sequence
            .checked_add(1)
            .ok_or(Failure::Integrity(Defect::CounterOverflow))?;
        let due = self.seals_since_checkpoint.saturating_add(1) >= CHECKPOINT_EVERY;
        let checkpoint = due.then(|| entries(&table));
        let seal = Seal {
            sequence,
            generation: self.generation,
            prev: self.last_seal,
            added,
            checkpoint,
            provenance: None,
            manifest: manifest.clone(),
        };
        self.point("pack-seal")?;
        let link = self.append_seal(&seal)?;
        self.point("pack-flush")?;
        if let Err(error) = self.writer.flush() {
            tracing::warn!(%error, "pack flush failed; writer poisoned");
            self.poisoned = true;
            return Err(Failure::OutcomeUnknown);
        }
        // I2: the bytes are durable; only now may a snapshot name them.
        self.snapshot = Arc::new(Snapshot {
            generation: self.snapshot.generation.clone(),
            table,
            sealed_len: self.writer.len(),
            sequence,
        });
        self.last_seal = Some(link);
        self.seals_since_checkpoint = if due {
            0
        } else {
            self.seals_since_checkpoint.saturating_add(1)
        };
        self.provenance = None;
        self.manifest = Some(manifest);
        Ok(sequence)
    }

    /// Copy every object `live` keeps into the next generation's slot under a
    /// fresh salt, seal it one generation up at the same sequence, retire
    /// this slot into the graveyard, and sweep whatever the graveyard holds
    /// that no view pins any more. Every copy is re-verified against its
    /// content address, so compaction can never launder a corrupt object into
    /// the new generation.
    pub fn compact(&mut self, live: &dyn Fn(&[u8; 32]) -> bool) -> Result<(), Failure> {
        if self.poisoned {
            return Err(Failure::OutcomeUnknown);
        }
        if self.last_seal.is_none() {
            // Nothing was ever committed: there is nothing to copy, and a
            // successor would only launder `None` into an empty manifest.
            return Ok(());
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(Failure::Integrity(Defect::CounterOverflow))?;
        let successor_name = slot_name(&self.prefix, generation);
        let _ = self.medium.remove_slot(&successor_name);
        let built = self.build_successor(&successor_name, generation, live);
        let Ok((writer, read, key, len, table, link)) = built else {
            // This pack is untouched and still authoritative; the successor
            // is garbage. No poison — the failure costs the compaction only.
            let _ = self.medium.remove_slot(&successor_name);
            return built.map(|_| ());
        };

        // --- The successor is now authoritative: nothing below may fail. ---
        let sequence = self.snapshot.sequence;
        self.writer = writer;
        self.key = key;
        self.generation = generation;
        self.last_seal = Some(link);
        self.seals_since_checkpoint = 0;
        let retired = std::mem::replace(
            &mut self.snapshot,
            Arc::new(Snapshot {
                generation: Arc::new(Generation {
                    name: successor_name,
                    read,
                }),
                table,
                sealed_len: len,
                sequence,
            }),
        );
        self.graveyard.push(retired.generation.clone());
        drop(retired);
        // Retirement is cleanup, and it waits for the readers: a generation
        // leaves the graveyard only when no view pins it. A crash here loses
        // nothing — the next open's election removes the losers.
        if !self.crash_requested("pack-compact-retire") {
            self.sweep_graveyard();
        }
        Ok(())
    }

    /// Remove every retired generation no view pins any more. Deletion is an
    /// act of the store on the store's thread — never a reader's side effect
    /// — and the handle closes before the name goes, which is also the only
    /// order OPFS accepts.
    pub fn sweep_graveyard(&mut self) {
        let mut kept = Vec::new();
        for generation in self.graveyard.drain(..) {
            // No new pins of a retired generation can be minted, so a count
            // of one — ours — only ever stays one.
            if Arc::strong_count(&generation) > 1 {
                kept.push(generation);
                continue;
            }
            let name = generation.name.clone();
            drop(generation);
            let _ = self.medium.remove_slot(&name);
        }
        self.graveyard = kept;
    }

    /// How many retired generations still wait on readers.
    #[must_use]
    pub fn graveyard_depth(&self) -> usize {
        self.graveyard.len()
    }

    #[allow(clippy::type_complexity, reason = "a one-caller bundle, not an API")]
    fn build_successor(
        &mut self,
        successor_name: &str,
        generation: u64,
        live: &dyn Fn(&[u8; 32]) -> bool,
    ) -> Result<
        (
            Box<dyn SlotWriter>,
            Arc<dyn ReadAt>,
            [u8; 32],
            u64,
            Table,
            SealLink,
        ),
        Failure,
    > {
        let (mut writer, read, key) = init_slot(self.medium.as_ref(), successor_name)?;
        self.point("pack-compact-objects")?;
        let mut table = Table::new();
        let keep: Vec<[u8; 32]> = self
            .snapshot
            .table
            .keys()
            .copied()
            .filter(|h| live(h))
            .collect();
        for hash in keep {
            let bytes = self.read(&hash)?;
            let offset = append_record(writer.as_mut(), &key, OBJECT_MAGIC, &bytes)?;
            let size = u64::try_from(bytes.len()).map_err(|_| corrupt())?;
            table.insert(hash, (offset, size));
        }
        let seal = Seal {
            sequence: self.snapshot.sequence,
            generation,
            prev: None,
            added: Vec::new(),
            checkpoint: Some(entries(&table)),
            provenance: None,
            manifest: self.manifest.clone().unwrap_or_default(),
        };
        self.point("pack-compact-seal")?;
        let payload = postcard::to_stdvec(&seal).map_err(|_| encode_failure())?;
        let record_offset = writer.len();
        append_record(writer.as_mut(), &key, SEAL_MAGIC, &payload)?;
        let link = SealLink {
            offset: record_offset,
            check: record_check(&key, SEAL_MAGIC, record_offset, &payload),
        };
        self.point("pack-compact-flush")?;
        writer
            .flush()
            .map_err(|error| io_err(Operation::Sync, &error))?;
        let len = writer.len();
        Ok((writer, read, key, len, table, link))
    }

    fn append_record(&mut self, magic: [u8; 4], payload: &[u8]) -> Result<u64, Failure> {
        append_record(self.writer.as_mut(), &self.key, magic, payload)
    }

    fn append_seal(&mut self, seal: &Seal) -> Result<SealLink, Failure> {
        let payload = postcard::to_stdvec(seal).map_err(|_| encode_failure())?;
        let record_offset = self.writer.len();
        self.append_record(SEAL_MAGIC, &payload)?;
        Ok(SealLink {
            offset: record_offset,
            check: record_check(&self.key, SEAL_MAGIC, record_offset, &payload),
        })
    }

    fn point(&self, name: &str) -> Result<(), Failure> {
        if let Some(injector) = &self.injector {
            if injector(name) {
                tracing::warn!(point = name, "pack fault injected");
                return Err(Failure::Operation {
                    operation: Operation::Write,
                    kind: IoKind::Interrupted,
                });
            }
        }
        Ok(())
    }

    fn crash_requested(&self, name: &str) -> bool {
        self.injector.as_ref().is_some_and(|i| i(name))
    }

    #[cfg(test)]
    pub(crate) fn seals_since_checkpoint(&self) -> u64 {
        self.seals_since_checkpoint
    }

    /// Where one object's payload lives: slot name, offset, length. A test
    /// seam — the way a corruption test reaches real bytes now that objects
    /// have no file of their own.
    #[cfg(any(test, feature = "fault-injection"))]
    #[must_use]
    pub fn object_location(&self, hash: &[u8; 32]) -> Option<(String, u64, u64)> {
        let (offset, len) = self.snapshot.table.get(hash).copied()?;
        Some((self.snapshot.generation.name.clone(), offset, len))
    }
}

fn slot_name(prefix: &str, generation: u64) -> String {
    format!("{prefix}-{generation}")
}

/// Every slot of one family present on the medium, by name.
fn slot_family(medium: &dyn Medium, prefix: &str) -> Result<Vec<String>, Failure> {
    let names = medium
        .slot_names()
        .map_err(|e| io_err(Operation::Open, &e))?;
    Ok(names
        .into_iter()
        .filter(|name| {
            name.strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|digits| digits.chars().all(|c| c.is_ascii_digit()))
        })
        .collect())
}

/// Remove every slot of one family — the recovery a stillborn pack gets when
/// its migration source is still authoritative. Never called while a store
/// serves the family.
pub(crate) fn remove_family(medium: &dyn Medium, prefix: &str) -> Result<(), Failure> {
    for name in slot_family(medium, prefix)? {
        medium
            .remove_slot(&name)
            .map_err(|e| io_err(Operation::Remove, &e))?;
    }
    Ok(())
}

fn entries(table: &Table) -> Vec<TableEntry> {
    table
        .iter()
        .map(|(hash, (offset, len))| TableEntry {
            hash: *hash,
            offset: *offset,
            len: *len,
        })
        .collect()
}

type OpenedSlot = (Box<dyn SlotWriter>, Arc<dyn ReadAt>, [u8; 32]);

fn init_slot(medium: &dyn Medium, name: &str) -> Result<OpenedSlot, Failure> {
    let (mut writer, read) = medium
        .open_slot(name)
        .map_err(|e| io_err(Operation::Open, &e))?;
    if writer.len() > 0 {
        writer
            .truncate(0)
            .map_err(|e| io_err(Operation::Open, &e))?;
    }
    let name_bytes = name.as_bytes();
    if name_bytes.len() > HEADER_NAME_CAPACITY {
        return Err(corrupt());
    }
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|_| Failure::Operation {
        operation: Operation::Write,
        kind: IoKind::Other,
    })?;
    let mut header = vec![0u8; usize::try_from(HEADER_LEN).unwrap_or(64)];
    let (magic_part, rest) = header.split_at_mut(SLOT_MAGIC.len());
    magic_part.copy_from_slice(SLOT_MAGIC);
    let (salt_part, rest) = rest.split_at_mut(SALT_LEN);
    salt_part.copy_from_slice(&salt);
    let (len_part, name_part) = rest.split_at_mut(2);
    len_part.copy_from_slice(&u16::try_from(name_bytes.len()).unwrap_or(0).to_le_bytes());
    if let Some(target) = name_part.get_mut(..name_bytes.len()) {
        target.copy_from_slice(name_bytes);
    }
    writer
        .append(&header)
        .map_err(|e| io_err(Operation::Write, &e))?;
    // The header must be durable before the first seal can claim anything:
    // a seal verified under a salt whose header never landed is unfindable.
    writer.flush().map_err(|e| io_err(Operation::Sync, &e))?;
    Ok((writer, read, blake3::derive_key(RECORD_KEY_CONTEXT, &salt)))
}

fn append_record(
    writer: &mut dyn SlotWriter,
    key: &[u8; 32],
    magic: [u8; 4],
    payload: &[u8],
) -> Result<u64, Failure> {
    let payload_len = u32::try_from(payload.len()).map_err(|_| corrupt())?;
    if payload_len > MAX_RECORD_BYTES {
        return Err(corrupt());
    }
    let offset = writer.len();
    let mut record = Vec::new();
    record.extend_from_slice(&magic);
    record.extend_from_slice(&payload_len.to_le_bytes());
    record.extend_from_slice(payload);
    record.extend_from_slice(&record_check(key, magic, offset, payload));
    writer
        .append(&record)
        .map_err(|e| io_err(Operation::Write, &e))?;
    // The returned offset addresses the payload, where reads begin.
    offset.checked_add(8).ok_or_else(corrupt)
}

/// Read and verify one record at `offset`; answers `(payload, end_offset)`.
fn read_record(
    read: &dyn ReadAt,
    key: &[u8; 32],
    slot_len: u64,
    offset: u64,
    expect: [u8; 4],
) -> Option<(Vec<u8>, u64)> {
    if offset.checked_add(8)? > slot_len {
        return None;
    }
    let mut head = [0u8; 8];
    read.read_at(offset, &mut head).ok()?;
    let (magic, len_bytes) = head.split_at(4);
    if magic != expect {
        return None;
    }
    let payload_len = u32::from_le_bytes(len_bytes.try_into().ok()?);
    if payload_len > MAX_RECORD_BYTES {
        return None;
    }
    let end = offset
        .checked_add(RECORD_OVERHEAD)?
        .checked_add(u64::from(payload_len))?;
    if end > slot_len {
        return None;
    }
    let mut payload = vec![0u8; usize::try_from(payload_len).ok()?];
    let payload_at = offset.checked_add(8)?;
    read.read_at(payload_at, &mut payload).ok()?;
    let mut check = [0u8; CHECK_LEN];
    read.read_at(payload_at.checked_add(u64::from(payload_len))?, &mut check)
        .ok()?;
    if record_check(key, expect, offset, &payload) != check {
        return None;
    }
    Some((payload, end))
}

fn recover_slot(
    name: String,
    writer: Box<dyn SlotWriter>,
    read: Arc<dyn ReadAt>,
) -> Result<RecoveredSlot, Failure> {
    let len = writer.len();
    if len < HEADER_LEN {
        return Ok(RecoveredSlot {
            name,
            writer,
            read,
            key: [0u8; 32],
            state: None,
            torn_birth: false,
        });
    }
    let mut header = [0u8; 64];
    read.read_at(0, &mut header)
        .map_err(|e| io_err(Operation::Read, &e))?;
    let (magic, rest) = header.split_at(SLOT_MAGIC.len());
    let (salt, rest) = rest.split_at(SALT_LEN);
    if magic != SLOT_MAGIC {
        // Not "never initialized" — that is the short-slot case above. Bytes
        // are here and they are not ours; say so before they are reset.
        tracing::warn!(slot = %name, "pack slot header unrecognized; resetting");
        return Ok(RecoveredSlot {
            name,
            writer,
            read,
            key: [0u8; 32],
            state: None,
            torn_birth: false,
        });
    }
    let named = rest
        .split_first_chunk::<2>()
        .and_then(|(len_bytes, name_part)| {
            let name_len = usize::from(u16::from_le_bytes(*len_bytes));
            name_part.get(..name_len)
        });
    if named != Some(name.as_bytes()) {
        // Bytes wearing another slot's name — a recycled physical file whose
        // truncation never became durable. History, not a slot.
        tracing::warn!(slot = %name, "pack slot header names a different slot; resetting");
        return Ok(RecoveredSlot {
            name,
            writer,
            read,
            key: [0u8; 32],
            state: None,
            torn_birth: false,
        });
    }
    let key = blake3::derive_key(RECORD_KEY_CONTEXT, salt);
    let (state, torn_birth) = recover_state(read.as_ref(), &key, len);
    if state.is_none() && len > HEADER_LEN {
        tracing::warn!(slot = %name, "pack slot holds bytes but no seal verifies");
    }
    Ok(RecoveredSlot {
        name,
        writer,
        read,
        key,
        state,
        torn_birth,
    })
}

/// Find the newest seal that verifies completely — its check, its chain, and
/// the content addresses of every object its delta names — stepping back one
/// seal each time verification fails. `None` means no commit survives.
///
/// A **birth seal** (`prev: None` — genesis, a compaction successor, a
/// migration) has no chain behind it: its single unflushed batch is the whole
/// slot, so its checkpoint entries are re-hashed too. Without that, a torn
/// successor would verify structurally, win the election on the generation
/// tiebreak, and take the intact predecessor down with it as a loser.
fn recover_state(read: &dyn ReadAt, key: &[u8; 32], len: u64) -> (Option<SlotState>, bool) {
    let birth_holds = |seal: &Seal| {
        seal.prev.is_some()
            || seal
                .checkpoint
                .as_deref()
                .is_none_or(|entries| delta_holds(read, entries))
    };
    let mut torn_birth = false;
    let mut candidate = newest_seal(read, key, len);
    while let Some((offset, seal, end)) = candidate {
        match verify_chain(read, key, len, &seal) {
            Some((table, seals_since_checkpoint)) => {
                if delta_holds(read, &seal.added) && birth_holds(&seal) {
                    let Some((payload, _)) = read_record(read, key, len, offset, SEAL_MAGIC) else {
                        return (None, torn_birth);
                    };
                    return (
                        Some(SlotState {
                            len: end,
                            table,
                            sequence: seal.sequence,
                            generation: seal.generation,
                            last_seal: SealLink {
                                offset,
                                check: record_check(key, SEAL_MAGIC, offset, &payload),
                            },
                            seals_since_checkpoint,
                            provenance: seal.provenance,
                            manifest: seal.manifest,
                        }),
                        torn_birth,
                    );
                }
                // A checkpoint-bearing chain root that fails its re-hash is
                // acknowledged history this slot can no longer expose — a
                // genesis delta seal (no checkpoint) is just an unacked tail.
                if seal.prev.is_none() && seal.checkpoint.is_some() {
                    torn_birth = true;
                }
            }
            None => {}
        }
        candidate = seal
            .prev
            .and_then(|link| read_seal(read, key, len, link.offset));
    }
    (None, torn_birth)
}

fn read_seal(read: &dyn ReadAt, key: &[u8; 32], len: u64, offset: u64) -> Option<(u64, Seal, u64)> {
    let (payload, end) = read_record(read, key, len, offset, SEAL_MAGIC)?;
    let seal: Seal = postcard::from_bytes(&payload).ok()?;
    Some((offset, seal, end))
}

/// Scan back from the end of the slot for the newest record that parses and
/// verifies as a seal. The keyed, offset-bound checks mean a candidate that
/// verifies is a record of this slot life at this position — never a copy
/// embedded in some object's payload; one that fails deeper verification is
/// stepped past by the caller.
fn newest_seal(read: &dyn ReadAt, key: &[u8; 32], len: u64) -> Option<(u64, Seal, u64)> {
    let magic_len = u64::try_from(SEAL_MAGIC.len()).ok()?;
    let mut window_end = len;
    loop {
        let window_start = window_end.saturating_sub(SCAN_WINDOW).max(HEADER_LEN);
        let size = usize::try_from(window_end.checked_sub(window_start)?).ok()?;
        let mut bytes = vec![0u8; size];
        read.read_at(window_start, &mut bytes).ok()?;
        let mut position = size;
        while position >= SEAL_MAGIC.len() {
            let start = position.checked_sub(SEAL_MAGIC.len())?;
            if bytes.get(start..position) == Some(SEAL_MAGIC.as_slice()) {
                let offset = window_start.checked_add(u64::try_from(start).ok()?)?;
                if let Some(found) = read_seal(read, key, len, offset) {
                    return Some(found);
                }
            }
            position = position.checked_sub(1)?;
        }
        if window_start == HEADER_LEN {
            return None;
        }
        // Overlap windows by the magic width so a boundary-split magic is seen.
        window_end = window_start.checked_add(magic_len.checked_sub(1)?)?;
    }
}

/// Walk the chain from `seal` back to its checkpoint (or genesis), verifying
/// each link's check and sequence order, and build the table the chain
/// describes. Answers `(table, seals since the last checkpoint)`.
fn verify_chain(read: &dyn ReadAt, key: &[u8; 32], len: u64, seal: &Seal) -> Option<(Table, u64)> {
    let mut deltas: Vec<Vec<TableEntry>> = vec![seal.added.clone()];
    let mut base = seal.checkpoint.clone();
    let mut from_checkpoint = base.is_some();
    let mut sequence = seal.sequence;
    let mut link = seal.prev;
    while base.is_none() {
        let Some(current) = link else {
            // Genesis: the chain starts from nothing.
            base = Some(Vec::new());
            break;
        };
        let (_, prior, _) = read_seal(read, key, len, current.offset)?;
        let (payload, _) = read_record(read, key, len, current.offset, SEAL_MAGIC)?;
        if record_check(key, SEAL_MAGIC, current.offset, &payload) != current.check {
            return None;
        }
        if prior.sequence >= sequence {
            return None;
        }
        sequence = prior.sequence;
        deltas.push(prior.added.clone());
        base.clone_from(&prior.checkpoint);
        from_checkpoint = base.is_some();
        link = prior.prev;
    }
    let mut table = Table::new();
    for entry in base? {
        table.insert(entry.hash, (entry.offset, entry.len));
    }
    for delta in deltas.iter().rev() {
        for entry in delta {
            table.insert(entry.hash, (entry.offset, entry.len));
        }
    }
    let walked = u64::try_from(deltas.len()).ok()?;
    // A checkpoint seal itself counts as zero-since; a genesis chain has
    // every one of its seals outstanding.
    let since = if from_checkpoint {
        walked.saturating_sub(1)
    } else {
        walked
    };
    Some((table, since))
}

/// The recovery rule the reordering model demands: a seal is a commit only if
/// every object its delta names re-hashes to its address.
fn delta_holds(read: &dyn ReadAt, added: &[TableEntry]) -> bool {
    added.iter().all(|entry| {
        let Ok(size) = usize::try_from(entry.len) else {
            return false;
        };
        let mut bytes = vec![0u8; size];
        if read.read_at(entry.offset, &mut bytes).is_err() {
            return false;
        }
        object_content_hash(&bytes) == entry.hash
    })
}
