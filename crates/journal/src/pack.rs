//! The pack log: the append-only physical format the store is moving to.
//!
//! One slot holds the live pack — a header, then OBJECT and SEAL records,
//! appended and never rewritten. A commit appends its objects and one seal,
//! then flushes **once**; everything up to the last verified seal is the
//! store's durable state. Where the current format spends a fsync per phase
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
//!   checksummed seal from a prior life of the file. Every record check is
//!   keyed by a per-slot random salt, and every seal names its predecessor's
//!   offset and check, so nothing from another life or another slot can join
//!   the chain. (SQLite's WAL salts and chained frame checksums, exactly.)
//! - **Generations are elected by sequence, not by generation.** Compaction
//!   copies live objects into the sibling slot and seals it one generation
//!   up. A crash can leave both slots valid; the one whose verified seal
//!   covers the highest sequence wins, generation only breaking the tie, so
//!   a durably committed sequence is never dropped by a half-finished
//!   compaction. Copying re-hashes every object, so compaction can never
//!   launder bit rot into a fresh generation.
//! - **A failed flush poisons the writer.** After fsyncgate, a retried flush
//!   that "succeeds" proves nothing. The commit is [`Failure::OutcomeUnknown`]
//!   and so is every call after it; reopening — a fresh handle and a full
//!   verified recovery — is the only way back.
//!
//! Open cost is O(seals since the last checkpoint), not O(pack): recovery
//! finds the newest seal by scanning back from the end, then walks the seal
//! chain, and every [`CHECKPOINT_EVERY`]th seal carries the full table so the
//! walk is bounded. Reads verify content addresses, as everywhere else.
//!
//! This module is not yet what [`crate::Store`] serves. It becomes the served
//! format when the semantic layer is rebound over it; until then it ships
//! with its own crash matrix ([`PACK_FAULT_POINTS`]) and carries no caller.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::medium::{Medium, Slot};
use crate::{object_content_hash, Defect, Failure, IoKind, Operation};

const SLOT_MAGIC: &[u8; 8] = b"laitpak1";
const SALT_LEN: usize = 16;
const HEADER_LEN: u64 = 24;
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
pub const PACK_FAULT_POINTS: &[&str] = &[
    "pack-objects",
    "pack-seal",
    "pack-flush",
    "pack-compact-objects",
    "pack-compact-seal",
    "pack-compact-flush",
    "pack-compact-retire",
];

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
    manifest: Vec<u8>,
}

type Table = BTreeMap<[u8; 32], (u64, u64)>;

/// The pack-log store engine: semantics-free, single-writer, one flush per
/// commit. `prefix` names the slot pair (`<prefix>-a`/`<prefix>-b`) so one
/// medium can carry a hot log and a cold one for large payloads, whose
/// compaction schedules must differ.
pub struct PackStore {
    medium: Arc<dyn Medium + Sync>,
    slot: Box<dyn Slot>,
    slot_name: String,
    prefix: String,
    key: [u8; 32],
    len: u64,
    table: Table,
    sequence: u64,
    generation: u64,
    last_seal: Option<SealLink>,
    seals_since_checkpoint: u64,
    manifest: Option<Vec<u8>>,
    poisoned: bool,
    injector: Option<crate::FaultInjector>,
}

impl std::fmt::Debug for PackStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackStore")
            .field("slot", &self.slot_name)
            .field("sequence", &self.sequence)
            .field("generation", &self.generation)
            .field("objects", &self.table.len())
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
    manifest: Vec<u8>,
}

struct RecoveredSlot {
    name: String,
    slot: Box<dyn Slot>,
    key: [u8; 32],
    state: Option<SlotState>,
}

impl PackStore {
    /// Open a pack pair, electing the slot whose verified seal covers the
    /// highest sequence (generation breaks ties), resetting the loser, and
    /// truncating the winner's unverified tail.
    pub fn open(medium: Arc<dyn Medium + Sync>, prefix: &str) -> Result<Self, Failure> {
        let names = medium
            .slot_names()
            .map_err(|e| io_err(Operation::Open, &e))?;
        let mut recovered: Vec<RecoveredSlot> = Vec::new();
        for suffix in ["a", "b"] {
            let name = format!("{prefix}-{suffix}");
            if !names.contains(&name) {
                continue;
            }
            let slot = medium
                .open_slot(&name)
                .map_err(|e| io_err(Operation::Open, &e))?;
            recovered.push(recover_slot(name, slot)?);
        }
        recovered.sort_by_key(|r| {
            r.state
                .as_ref()
                .map_or((0, 0), |s| (s.sequence, s.generation))
        });
        let elected = recovered.pop().filter(|r| r.state.is_some());
        // Losers and unusable slots are retired: their handle first, then
        // their name. A failed removal costs a retry at the next open.
        for loser in recovered {
            drop(loser.slot);
            let _ = medium.remove_slot(&loser.name);
        }
        let Some(RecoveredSlot {
            name,
            mut slot,
            key,
            state: Some(state),
        }) = elected
        else {
            // No slot holds a verified seal: initialize a fresh pack. Any
            // unverified bytes (a crash during first init) were discarded
            // with their slot above.
            let name = format!("{prefix}-a");
            let _ = medium.remove_slot(&name);
            let (slot, key, len) = init_slot(medium.as_ref(), &name)?;
            return Ok(Self {
                medium,
                slot,
                slot_name: name,
                prefix: prefix.to_owned(),
                key,
                len,
                table: Table::new(),
                sequence: 0,
                generation: 0,
                last_seal: None,
                seals_since_checkpoint: 0,
                manifest: None,
                poisoned: false,
                injector: None,
            });
        };
        let held = slot.len().map_err(|e| io_err(Operation::Open, &e))?;
        if held > state.len {
            slot.truncate(state.len)
                .map_err(|e| io_err(Operation::Open, &e))?;
            // Without this flush the truncate could reorder against the next
            // commit's appends and leave the old tail alive behind them.
            slot.flush().map_err(|e| io_err(Operation::Sync, &e))?;
        }
        Ok(Self {
            medium,
            slot,
            slot_name: name,
            prefix: prefix.to_owned(),
            key,
            len: state.len,
            table: state.table,
            sequence: state.sequence,
            generation: state.generation,
            last_seal: Some(state.last_seal),
            seals_since_checkpoint: state.seals_since_checkpoint,
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

    /// The manifest bytes sealed by the last commit, `None` for a fresh pack.
    #[must_use]
    pub fn manifest(&self) -> Option<&[u8]> {
        self.manifest.as_deref()
    }

    /// The last committed sequence; 0 for a fresh pack.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.table.contains_key(hash)
    }

    /// Read one object, verified against its content address.
    pub fn read(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Failure> {
        let (offset, len) = self
            .table
            .get(hash)
            .copied()
            .ok_or(Failure::Integrity(Defect::MissingObject))?;
        let capacity = usize::try_from(len).map_err(|_| corrupt())?;
        let mut bytes = vec![0u8; capacity];
        self.slot
            .read_at(offset, &mut bytes)
            .map_err(|e| io_err(Operation::Read, &e))?;
        if object_content_hash(&bytes) != *hash {
            return Err(Failure::Integrity(Defect::CorruptObject));
        }
        Ok(bytes)
    }

    /// Append `objects` and one seal carrying `manifest`, then flush once.
    /// Returns the committed sequence. A flush failure is
    /// [`Failure::OutcomeUnknown`] and poisons the writer: every later call
    /// answers the same until the pack is reopened.
    pub fn commit(&mut self, objects: &[Vec<u8>], manifest: Vec<u8>) -> Result<u64, Failure> {
        if self.poisoned {
            return Err(Failure::OutcomeUnknown);
        }
        let undo = self.len;
        match self.commit_inner(objects, manifest) {
            Ok(sequence) => Ok(sequence),
            Err(Failure::OutcomeUnknown) => Err(Failure::OutcomeUnknown),
            Err(failure) => {
                // The seal never flushed, so nothing was committed; drop the
                // partial tail now if the medium will let us, or poison and
                // let recovery discard it the same way. The truncate needs no
                // flush of its own: the next commit flushes before claiming
                // anything, and until then nothing references the tail.
                if self.slot.truncate(undo).is_ok() {
                    self.len = undo;
                } else {
                    self.poisoned = true;
                }
                Err(failure)
            }
        }
    }

    fn commit_inner(&mut self, objects: &[Vec<u8>], manifest: Vec<u8>) -> Result<u64, Failure> {
        self.point("pack-objects")?;
        let mut added = Vec::new();
        let mut batch = BTreeSet::new();
        for bytes in objects {
            let hash = object_content_hash(bytes);
            if self.table.contains_key(&hash) || !batch.insert(hash) {
                continue;
            }
            let offset = self.append_record(OBJECT_MAGIC, bytes)?;
            let len = u64::try_from(bytes.len()).map_err(|_| corrupt())?;
            added.push(TableEntry { hash, offset, len });
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(Failure::Integrity(Defect::CounterOverflow))?;
        let due = self.seals_since_checkpoint.saturating_add(1) >= CHECKPOINT_EVERY;
        let checkpoint = due.then(|| {
            let mut full = entries(&self.table);
            full.extend_from_slice(&added);
            full
        });
        let seal = Seal {
            sequence,
            generation: self.generation,
            prev: self.last_seal,
            added: added.clone(),
            checkpoint,
            manifest: manifest.clone(),
        };
        self.point("pack-seal")?;
        let link = self.append_seal(&seal)?;
        self.point("pack-flush")?;
        if let Err(error) = self.slot.flush() {
            tracing::warn!(%error, "pack flush failed; writer poisoned");
            self.poisoned = true;
            return Err(Failure::OutcomeUnknown);
        }
        for entry in added {
            self.table.insert(entry.hash, (entry.offset, entry.len));
        }
        self.sequence = sequence;
        self.last_seal = Some(link);
        self.seals_since_checkpoint = if due {
            0
        } else {
            self.seals_since_checkpoint.saturating_add(1)
        };
        self.manifest = Some(manifest);
        Ok(sequence)
    }

    /// Copy every object `live` keeps into the sibling slot under a fresh
    /// salt, seal it one generation up at the same sequence, and retire this
    /// slot. Every copy is re-verified against its content address, so
    /// compaction can never launder a corrupt object into the new generation.
    pub fn compact(&mut self, live: &dyn Fn(&[u8; 32]) -> bool) -> Result<(), Failure> {
        if self.poisoned {
            return Err(Failure::OutcomeUnknown);
        }
        if self.last_seal.is_none() {
            // Nothing was ever committed: there is nothing to copy, and a
            // successor would only launder `None` into an empty manifest.
            return Ok(());
        }
        let successor_name = self.sibling_name();
        let _ = self.medium.remove_slot(&successor_name);
        let built = self.build_successor(&successor_name, live);
        let Ok((successor, key, len, table, link, generation)) = built else {
            // This pack is untouched and still authoritative; the successor
            // is garbage. No poison — the failure costs the compaction only.
            let _ = self.medium.remove_slot(&successor_name);
            return built.map(|_| ());
        };

        // --- The successor is now authoritative: nothing below may fail. ---
        let retired = std::mem::replace(&mut self.slot, successor);
        let retired_name = std::mem::replace(&mut self.slot_name, successor_name);
        self.key = key;
        self.len = len;
        self.table = table;
        self.generation = generation;
        self.last_seal = Some(link);
        self.seals_since_checkpoint = 0;
        // Retirement is cleanup: the handle must close before the name goes,
        // and a lost removal is re-resolved by the next open's election.
        if !self.crash_requested("pack-compact-retire") {
            drop(retired);
            let _ = self.medium.remove_slot(&retired_name);
        }
        Ok(())
    }

    /// Everything compaction does before the authority switch: copy the live
    /// objects (re-verified by [`Self::read`]), seal, flush. On any failure
    /// the caller discards the successor whole.
    #[allow(clippy::type_complexity, reason = "a one-caller bundle, not an API")]
    fn build_successor(
        &mut self,
        successor_name: &str,
        live: &dyn Fn(&[u8; 32]) -> bool,
    ) -> Result<(Box<dyn Slot>, [u8; 32], u64, Table, SealLink, u64), Failure> {
        let (mut successor, key, mut len) = init_slot(self.medium.as_ref(), successor_name)?;
        self.point("pack-compact-objects")?;
        let mut table = Table::new();
        let keep: Vec<[u8; 32]> = self.table.keys().copied().filter(|h| live(h)).collect();
        for hash in keep {
            let bytes = self.read(&hash)?;
            let offset = append_record(successor.as_mut(), &key, &mut len, OBJECT_MAGIC, &bytes)?;
            let size = u64::try_from(bytes.len()).map_err(|_| corrupt())?;
            table.insert(hash, (offset, size));
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(Failure::Integrity(Defect::CounterOverflow))?;
        let seal = Seal {
            sequence: self.sequence,
            generation,
            prev: None,
            added: Vec::new(),
            checkpoint: Some(entries(&table)),
            manifest: self.manifest.clone().unwrap_or_default(),
        };
        self.point("pack-compact-seal")?;
        let payload = postcard::to_stdvec(&seal).map_err(|_| encode_failure())?;
        let record_offset = len;
        append_record(successor.as_mut(), &key, &mut len, SEAL_MAGIC, &payload)?;
        let link = SealLink {
            offset: record_offset,
            check: record_check(&key, SEAL_MAGIC, record_offset, &payload),
        };
        self.point("pack-compact-flush")?;
        successor
            .flush()
            .map_err(|error| io_err(Operation::Sync, &error))?;
        Ok((successor, key, len, table, link, generation))
    }

    fn sibling_name(&self) -> String {
        let a = format!("{}-a", self.prefix);
        if self.slot_name == a {
            format!("{}-b", self.prefix)
        } else {
            a
        }
    }

    fn append_record(&mut self, magic: [u8; 4], payload: &[u8]) -> Result<u64, Failure> {
        append_record(self.slot.as_mut(), &self.key, &mut self.len, magic, payload)
    }

    fn append_seal(&mut self, seal: &Seal) -> Result<SealLink, Failure> {
        let payload = postcard::to_stdvec(seal).map_err(|_| encode_failure())?;
        let record_offset = self.len;
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

/// A freshly initialized slot: its handle, its derived record key, and its
/// length (the header).
type OpenedSlot = (Box<dyn Slot>, [u8; 32], u64);

fn init_slot(medium: &(dyn Medium + Sync), name: &str) -> Result<OpenedSlot, Failure> {
    let mut slot = medium
        .open_slot(name)
        .map_err(|e| io_err(Operation::Open, &e))?;
    if !slot.is_empty().map_err(|e| io_err(Operation::Open, &e))? {
        slot.truncate(0).map_err(|e| io_err(Operation::Open, &e))?;
    }
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|_| Failure::Operation {
        operation: Operation::Write,
        kind: IoKind::Other,
    })?;
    let mut header = Vec::new();
    header.extend_from_slice(SLOT_MAGIC);
    header.extend_from_slice(&salt);
    slot.append(&header)
        .map_err(|e| io_err(Operation::Write, &e))?;
    // The header must be durable before the first seal can claim anything:
    // a seal verified under a salt whose header never landed is unfindable.
    slot.flush().map_err(|e| io_err(Operation::Sync, &e))?;
    Ok((
        slot,
        blake3::derive_key(RECORD_KEY_CONTEXT, &salt),
        HEADER_LEN,
    ))
}

fn append_record(
    slot: &mut dyn Slot,
    key: &[u8; 32],
    len: &mut u64,
    magic: [u8; 4],
    payload: &[u8],
) -> Result<u64, Failure> {
    let payload_len = u32::try_from(payload.len()).map_err(|_| corrupt())?;
    if payload_len > MAX_RECORD_BYTES {
        return Err(corrupt());
    }
    let mut record = Vec::new();
    record.extend_from_slice(&magic);
    record.extend_from_slice(&payload_len.to_le_bytes());
    record.extend_from_slice(payload);
    record.extend_from_slice(&record_check(key, magic, *len, payload));
    let offset = slot
        .append(&record)
        .map_err(|e| io_err(Operation::Write, &e))?;
    if offset != *len {
        // The tracked length and the medium disagree — the slot has been
        // touched by something else. Refuse rather than interleave.
        return Err(corrupt());
    }
    *len = len
        .checked_add(u64::try_from(record.len()).map_err(|_| corrupt())?)
        .ok_or_else(corrupt)?;
    // The returned offset addresses the payload, where reads begin.
    offset.checked_add(8).ok_or_else(corrupt)
}

/// Read and verify one record at `offset`; answers `(payload, end_offset)`.
fn read_record(
    slot: &dyn Slot,
    key: &[u8; 32],
    slot_len: u64,
    offset: u64,
    expect: [u8; 4],
) -> Option<(Vec<u8>, u64)> {
    if offset.checked_add(8)? > slot_len {
        return None;
    }
    let mut head = [0u8; 8];
    slot.read_at(offset, &mut head).ok()?;
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
    slot.read_at(payload_at, &mut payload).ok()?;
    let mut check = [0u8; CHECK_LEN];
    slot.read_at(payload_at.checked_add(u64::from(payload_len))?, &mut check)
        .ok()?;
    if record_check(key, expect, offset, &payload) != check {
        return None;
    }
    Some((payload, end))
}

fn recover_slot(name: String, slot: Box<dyn Slot>) -> Result<RecoveredSlot, Failure> {
    let unusable = |slot| RecoveredSlot {
        name: name.clone(),
        slot,
        key: [0u8; 32],
        state: None,
    };
    let len = slot.len().map_err(|e| io_err(Operation::Open, &e))?;
    if len < HEADER_LEN {
        return Ok(unusable(slot));
    }
    let mut header = [0u8; 24];
    slot.read_at(0, &mut header)
        .map_err(|e| io_err(Operation::Read, &e))?;
    let (magic, salt) = header.split_at(SLOT_MAGIC.len());
    if magic != SLOT_MAGIC {
        // Not "never initialized" — that is the short-slot case above. Bytes
        // are here and they are not ours; say so before they are reset.
        tracing::warn!(slot = %name, "pack slot header unrecognized; resetting");
        return Ok(unusable(slot));
    }
    let key = blake3::derive_key(RECORD_KEY_CONTEXT, salt);
    let state = recover_state(slot.as_ref(), &key, len);
    if state.is_none() && len > HEADER_LEN {
        tracing::warn!(slot = %name, "pack slot holds bytes but no seal verifies; resetting");
    }
    Ok(RecoveredSlot {
        name,
        slot,
        key,
        state,
    })
}

/// Find the newest seal that verifies completely — its check, its chain, and
/// the content addresses of every object its delta names — stepping back one
/// seal each time verification fails. `None` means no commit survives.
fn recover_state(slot: &dyn Slot, key: &[u8; 32], len: u64) -> Option<SlotState> {
    let mut candidate = newest_seal(slot, key, len);
    while let Some((offset, seal, end)) = candidate {
        match verify_chain(slot, key, len, &seal) {
            Some((table, seals_since_checkpoint)) if delta_holds(slot, &seal.added) => {
                let (payload, _) = read_record(slot, key, len, offset, SEAL_MAGIC)?;
                return Some(SlotState {
                    len: end,
                    table,
                    sequence: seal.sequence,
                    generation: seal.generation,
                    last_seal: SealLink {
                        offset,
                        check: record_check(key, SEAL_MAGIC, offset, &payload),
                    },
                    seals_since_checkpoint,
                    manifest: seal.manifest,
                });
            }
            _ => {
                candidate = seal
                    .prev
                    .and_then(|link| read_seal(slot, key, len, link.offset));
            }
        }
    }
    None
}

fn read_seal(slot: &dyn Slot, key: &[u8; 32], len: u64, offset: u64) -> Option<(u64, Seal, u64)> {
    let (payload, end) = read_record(slot, key, len, offset, SEAL_MAGIC)?;
    let seal: Seal = postcard::from_bytes(&payload).ok()?;
    Some((offset, seal, end))
}

/// Scan back from the end of the slot for the newest record that parses and
/// verifies as a seal. The keyed, offset-bound checks mean a candidate that
/// verifies is a record of this slot life at this position — never a copy
/// embedded in some object's payload; one that fails deeper verification is
/// stepped past by the caller.
fn newest_seal(slot: &dyn Slot, key: &[u8; 32], len: u64) -> Option<(u64, Seal, u64)> {
    let magic_len = u64::try_from(SEAL_MAGIC.len()).ok()?;
    let mut window_end = len;
    loop {
        let window_start = window_end.saturating_sub(SCAN_WINDOW).max(HEADER_LEN);
        let size = usize::try_from(window_end.checked_sub(window_start)?).ok()?;
        let mut bytes = vec![0u8; size];
        slot.read_at(window_start, &mut bytes).ok()?;
        let mut position = size;
        while position >= SEAL_MAGIC.len() {
            let start = position.checked_sub(SEAL_MAGIC.len())?;
            if bytes.get(start..position) == Some(SEAL_MAGIC.as_slice()) {
                let offset = window_start.checked_add(u64::try_from(start).ok()?)?;
                if let Some(found) = read_seal(slot, key, len, offset) {
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
fn verify_chain(slot: &dyn Slot, key: &[u8; 32], len: u64, seal: &Seal) -> Option<(Table, u64)> {
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
        let (_, prior, _) = read_seal(slot, key, len, current.offset)?;
        let (payload, _) = read_record(slot, key, len, current.offset, SEAL_MAGIC)?;
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
fn delta_holds(slot: &dyn Slot, added: &[TableEntry]) -> bool {
    added.iter().all(|entry| {
        let Ok(size) = usize::try_from(entry.len) else {
            return false;
        };
        let mut bytes = vec![0u8; size];
        if slot.read_at(entry.offset, &mut bytes).is_err() {
            return false;
        }
        object_content_hash(&bytes) == entry.hash
    })
}
