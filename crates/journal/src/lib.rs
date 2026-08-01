#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! The journaled durable store — the semantics-free on-disk commit protocol.
//!
//! This crate knows only immutable content-addressed objects, object
//! references, an atomically swapped manifest with opaque caller metadata,
//! fsync/directory-sync discipline, fault injection, and recovery. It knows
//! nothing about Bodies, authority, Worlds, or any product — both the Engine
//! Body store and the mechanics authority ledger commit through it.
//!
//! Layout, under one store root (which may also hold caller-owned lifecycle
//! files — this crate touches only its own names):
//!
//! ```text
//! counter            // the local transaction counter (reserved + fsynced first)
//! current-manifest   // postcard Manifest, atomically replaced
//! objects/<hex64>    // immutable content-addressed objects
//! journal/active     // the active journal record, atomically replaced
//! ```
//!
//! A commit executes the normative sequence:
//!
//! 1. reserve the local transaction counter and fsync it (gaps after failure
//!    are allowed; **reuse is forbidden**);
//! 2. write/fsync journal `Prepared { new objects, new manifest hash }`;
//! 3. write/fsync all temporary objects;
//! 4. write/fsync `MaterialReady`, rename the immutable objects to their final
//!    paths, and fsync their directory;
//! 5. write/fsync the new manifest temp, rename it over `current-manifest`
//!    **last**, and fsync the store directory;
//! 6. write/fsync journal `Committed`, return, then remove the journal and
//!    fsync its directory.
//!
//! Recovery on open exposes **the complete old or the complete new** state:
//! `Prepared`/`MaterialReady` found with the old manifest removes the safe
//! orphan temps/objects and exposes the old state; `MaterialReady` found with
//! the new manifest verifies it completely and finalizes it as committed. A
//! manifest naming absent or corrupt objects is an integrity failure — never
//! repaired heuristically. Unreferenced objects are garbage-collected only
//! after recovery, when no journal is active.
//!
//! **The required set is an index, not a vector.** A manifest used to carry
//! every required `Object` inline, so a commit re-encoded and fsynced the
//! whole list to change one object — 28.8 MB at 100,000 Bodies, measured in
//! `benchmarks/commit-cost-baseline.md`. It now carries a root hash into a
//! canonical radix index (see [`index`]), and a commit rewrites only the paths
//! its changed objects touch. The index's own nodes are objects too, kept alive
//! by reachability from the root rather than by being entries in it.
//!
//! Every write/fsync/rename boundary carries a named fault-injection point so
//! the crash matrix is testable; see [`Store::with_fault_injector`].

mod index;

#[cfg(test)]
extern crate self as journal;
#[cfg(test)]
mod crash_tests;
#[cfg(test)]
mod fault_tests;
#[cfg(test)]
mod index_tests;
#[cfg(test)]
mod reconciliation_tests;

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const COUNTER_FILE: &str = "counter";
const MANIFEST_FILE: &str = "current-manifest";
const OBJECTS_DIR: &str = "objects";
const JOURNAL_DIR: &str = "journal";
const JOURNAL_FILE: &str = "active";

/// Domain for an object's content address.
const OBJECT_DOMAIN: &[u8] = b"lait/store-object/1";
/// Domain for a manifest's identity hash (referenced by the journal).
const MANIFEST_DOMAIN: &[u8] = b"lait/store-manifest/1";

/// Why a journal operation failed. The taxonomy is deliberately small: a
/// durable-write failure (retry may help after the cause clears), an integrity
/// failure (never repaired heuristically), and the one genuinely ambiguous
/// post-switch outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Operation {
        operation: Operation,
        kind: IoKind,
    },
    Integrity(Defect),
    /// The authoritative switch happened but its durability confirmation
    /// failed: the commit may or may not survive power loss. Fail stop and
    /// reopen — recovery resolves the outcome deterministically from the
    /// on-disk manifest. Never retry through this error.
    OutcomeUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Open,
    Read,
    Write,
    Sync,
    Rename,
    Remove,
    Encode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoKind {
    NotFound,
    PermissionDenied,
    Interrupted,
    InvalidData,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Defect {
    MissingObject,
    CorruptObject,
    CorruptManifest,
    CorruptJournal,
    MissingCounter,
    CorruptIndex,
    UnsupportedFormat,
    CounterOverflow,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operation { operation, kind } => write!(f, "{operation:?}: {kind:?}"),
            Self::Integrity(defect) => write!(f, "integrity: {defect:?}"),
            Self::OutcomeUnknown => write!(f, "outcome unknown"),
        }
    }
}
impl std::error::Error for Failure {}

/// One immutable object reference: content address and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Object {
    pub hash: [u8; 32],
    pub len: u64,
}

/// The encoded generation of the store format. The clean break lives here: an
/// older store is refused at open, never upgraded.
pub const STORE_FORMAT_VERSION: u8 = 2;

/// A caller's own authenticated index, as one commit sees it.
///
/// The two halves travel together because separating them corrupts silently:
/// roots without nodes name material the commit never wrote, and nodes without
/// roots are unreachable the moment they land.
#[derive(Debug, Clone, Copy)]
pub struct Index<'a> {
    /// The roots to record in the manifest. Replaces the previous set whole.
    pub roots: &'a [([u8; 32], u64)],
    /// The nodes this commit produced. Written, never made required.
    pub nodes: &'a [Vec<u8>],
}

impl Index<'_> {
    /// For a caller that keeps no index of its own.
    pub const NONE: Self = Self {
        roots: &[],
        nodes: &[],
    };
}

/// The store's indexed commit point: a root into the required-object index plus
/// a reference to opaque caller metadata. Both are small and neither grows with
/// the store, which is the entire difference from the shape this replaced.
///
/// The caller keeps its own large maps as index roots, and the store keeps
/// their nodes alive by reachability from those roots — that is how a
/// semantics-free journal preserves a Replica's Body catalog without being able
/// to read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u8,
    pub sequence: u64,
    /// Root of the index mapping required object hash to object length.
    /// `None` when nothing is required yet.
    pub required_object_index_root: Option<([u8; 32], u64)>,
    /// The caller's opaque metadata, as an object rather than inline.
    pub caller_meta: Option<Object>,
    /// Index roots the caller owns, kept alive by reachability.
    ///
    /// A caller keeps its own large maps as indexes — the Replica's Body
    /// catalog is the reason this exists — and their nodes are objects in this
    /// store. Making the caller name every node as it changed would be
    /// O(nodes) per commit, which is exactly the cost being removed. Naming the
    /// *roots* is O(1) and the sweep traverses them.
    ///
    /// This does not make the journal semantic: it knows these are indexes in
    /// the format it defines, and nothing about what they hold.
    pub caller_index_roots: Vec<([u8; 32], u64)>,
}

fn child(root: ([u8; 32], u64)) -> index::ChildRef {
    index::ChildRef {
        hash: root.0,
        count: root.1,
    }
}

fn coordinate(root: index::ChildRef) -> ([u8; 32], u64) {
    (root.hash, root.count)
}

fn required_root(manifest: &Manifest) -> Option<index::ChildRef> {
    manifest.required_object_index_root.map(child)
}

/// The journal phases. Each replaces `journal/active` atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum JournalRecord {
    Prepared {
        sequence: u64,
        new_objects: Vec<Object>,
        new_manifest_hash: [u8; 32],
    },
    MaterialReady {
        sequence: u64,
        new_objects: Vec<Object>,
        new_manifest_hash: [u8; 32],
    },
    Committed {
        sequence: u64,
        new_manifest_hash: [u8; 32],
    },
}

type FaultInjector = Box<dyn Fn(&str) -> bool + Send>;

/// The named fault points, in commit order (each fires before its operation).
#[cfg(any(test, feature = "fault-injection"))]
pub const FAULT_POINTS: [&str; 9] = [
    "counter",
    "journal-prepared",
    "objects",
    "journal-material-ready",
    "rename-objects",
    "manifest-temp",
    "manifest-rename",
    "journal-committed",
    "journal-remove",
];

/// The journaled store engine.
pub struct Store {
    root: PathBuf,
    manifest: Option<Manifest>,
    injector: Option<FaultInjector>,
    commits_since_sweep: u32,
}

/// How many commits may pass before the store collects what they orphaned.
///
/// A sweep walks the object directory once, so doing it per commit would put an
/// O(live) cost on an O(changed) operation. Amortising it over this many
/// commits keeps the average near zero while bounding how far a long-running
/// session can drift from live state.
const COMMITS_PER_SWEEP: u32 = 256;

// The injector closure is not `Debug`; show the root + current manifest.
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

fn object_hash(bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(OBJECT_DOMAIN);
    h.update(bytes);
    *h.finalize().as_bytes()
}

/// The content address the store gives a byte object — public so a caller can
/// predict the [`Object`] of material it hands to [`Store::commit`]
/// (e.g. a caller's meta index referencing the objects of the same commit).
#[must_use]
pub fn object_content_hash(bytes: &[u8]) -> [u8; 32] {
    object_hash(bytes)
}

fn manifest_hash(bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(MANIFEST_DOMAIN);
    h.update(bytes);
    *h.finalize().as_bytes()
}

fn hex(hash: &[u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(hash)
}

fn unhex(name: &str) -> Option<[u8; 32]> {
    let raw = data_encoding::HEXLOWER.decode(name.as_bytes()).ok()?;
    <[u8; 32]>::try_from(raw.as_slice()).ok()
}

/// An object's length, as the required index stores it.
fn encode_len(len: u64) -> Vec<u8> {
    len.to_be_bytes().to_vec()
}

fn decode_len(value: &[u8]) -> Option<u64> {
    <[u8; 8]>::try_from(value).ok().map(u64::from_be_bytes)
}

/// Reads index nodes out of the object directory. Index nodes are ordinary
/// content-addressed objects; what makes them nodes is that a root reaches them.
struct ObjectNodes<'a> {
    root: &'a Path,
}

impl index::NodeSource for ObjectNodes<'_> {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.root.join(OBJECTS_DIR).join(hex(hash))).ok()?;
        (object_hash(&bytes) == *hash).then_some(bytes)
    }
}

pub(crate) fn io_err(operation: Operation, error: std::io::Error) -> Failure {
    tracing::warn!(%error, ?operation, "journal operation failed");
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => IoKind::NotFound,
        std::io::ErrorKind::PermissionDenied => IoKind::PermissionDenied,
        std::io::ErrorKind::Interrupted => IoKind::Interrupted,
        std::io::ErrorKind::InvalidData => IoKind::InvalidData,
        _ => IoKind::Other,
    };
    Failure::Operation { operation, kind }
}

pub(crate) fn write_sync(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| io_err(Operation::Open, e))?;
    f.write_all(bytes)
        .map_err(|e| io_err(Operation::Write, e))?;
    f.sync_all().map_err(|e| io_err(Operation::Sync, e))?;
    Ok(())
}

/// Atomic replace with a brief retry for Windows sharing violations.
pub(crate) fn atomic_replace(tmp: &Path, dst: &Path) -> Result<(), Failure> {
    let mut last = None;
    for attempt in 0..5 {
        match std::fs::rename(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
                }
            }
        }
    }
    match last {
        Some(error) => Err(io_err(Operation::Rename, error)),
        None => Err(Failure::Operation {
            operation: Operation::Rename,
            kind: IoKind::Other,
        }),
    }
}

/// Directory durability after a rename/create. On unix this is a real fsync of
/// the directory, and a failure fails the calling phase. On Windows, a
/// directory handle needs `FILE_FLAG_BACKUP_SEMANTICS` to open; if no handle
/// can be opened at all the platform does not expose directory sync to us and
/// NTFS's metadata journaling is the documented durability contract — but a
/// handle that opens and then fails to flush is a real error and fails the
/// phase.
#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> Result<(), Failure> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_err(Operation::Sync, e))
}

#[cfg(windows)]
fn sync_dir(dir: &Path) -> Result<(), Failure> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .or_else(|_| {
            OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(dir)
        });
    match handle {
        // No directory handle at all: sync is unsupported here; NTFS metadata
        // journaling is the stated contract (documented, not silent).
        Err(_) => Ok(()),
        Ok(d) => d.sync_all().map_err(|e| io_err(Operation::Sync, e)),
    }
}

impl Store {
    /// Open a store root, running crash recovery, and return the store plus its
    /// current manifest (`None` for a fresh store). The exposed state is always
    /// the complete old or complete new one.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Failure> {
        let root = root.into();
        std::fs::create_dir_all(root.join(OBJECTS_DIR)).map_err(|e| io_err(Operation::Open, e))?;
        std::fs::create_dir_all(root.join(JOURNAL_DIR)).map_err(|e| io_err(Operation::Open, e))?;
        let mut store = Self {
            root,
            manifest: None,
            injector: None,
            commits_since_sweep: 0,
        };
        store.recover()?;
        Ok(store)
    }

    /// Attach a fault injector (test seam; see [`FAULT_POINTS`]).
    #[must_use]
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn with_fault_injector(mut self, injector: Box<dyn Fn(&str) -> bool + Send>) -> Self {
        self.injector = Some(injector);
        self
    }

    /// Attach a fault injector by reference (test seam for callers embedding
    /// the store; see [`FAULT_POINTS`]).
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn set_fault_injector(&mut self, injector: Box<dyn Fn(&str) -> bool + Send>) {
        self.injector = Some(injector);
    }

    /// The current manifest, if any commit has completed.
    #[must_use]
    pub const fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// Every currently required object, in key order.
    ///
    /// O(total required) by construction, so it is a diagnostic and a test
    /// affordance rather than something a commit path should call. Ask
    /// [`Self::is_required`] about one object instead.
    pub fn required_objects(&self) -> Result<Vec<Object>, Failure> {
        let Some(manifest) = &self.manifest else {
            return Ok(Vec::new());
        };
        let source = ObjectNodes { root: &self.root };
        let mut out = Vec::new();
        index::stream(&source, required_root(manifest), &mut |entry| {
            if let Some(len) = decode_len(&entry.value) {
                out.push(Object {
                    hash: entry.key,
                    len,
                });
            }
        })
        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        Ok(out)
    }

    /// Whether one object is currently required. O(index depth).
    pub fn is_required(&self, hash: &[u8; 32]) -> Result<bool, Failure> {
        let Some(manifest) = &self.manifest else {
            return Ok(false);
        };
        let source = ObjectNodes { root: &self.root };
        index::lookup(&source, required_root(manifest), hash)
            .map(|v| v.is_some())
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))
    }

    /// The caller's opaque metadata from the current commit point. It lives as
    /// an object rather than inline, so a manifest stays small no matter how
    /// much the caller keeps there.
    pub fn caller_meta(&self) -> Result<Option<Vec<u8>>, Failure> {
        match self.manifest.as_ref().and_then(|m| m.caller_meta) {
            None => Ok(None),
            Some(reference) => self.read_object(&reference).map(Some),
        }
    }

    /// Read an immutable object, verifying its content address.
    pub fn read_object(&self, obj: &Object) -> Result<Vec<u8>, Failure> {
        let bytes = self.read(&obj.hash)?;
        let length =
            u64::try_from(bytes.len()).map_err(|_| Failure::Integrity(Defect::CorruptObject))?;
        if length != obj.len {
            return Err(Failure::Integrity(Defect::CorruptObject));
        }
        Ok(bytes)
    }

    /// Read one immutable object by its content address.
    pub fn read(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Failure> {
        let path = self.object_path(hash);
        let bytes = std::fs::read(&path).map_err(|error| {
            tracing::warn!(%error, object = %hex(hash), "journal object is absent");
            Failure::Integrity(Defect::MissingObject)
        })?;
        if object_hash(&bytes) != *hash {
            return Err(Failure::Integrity(Defect::CorruptObject));
        }
        Ok(bytes)
    }

    fn object_path(&self, hash: &[u8; 32]) -> PathBuf {
        self.root.join(OBJECTS_DIR).join(hex(hash))
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join(JOURNAL_DIR).join(JOURNAL_FILE)
    }

    fn point(&self, name: &str) -> Result<(), Failure> {
        if let Some(injector) = &self.injector {
            if injector(name) {
                tracing::warn!(point = name, "journal fault injected");
                return Err(Failure::Operation {
                    operation: Operation::Write,
                    kind: IoKind::Interrupted,
                });
            }
        }
        Ok(())
    }

    fn write_journal(&self, record: &JournalRecord) -> Result<(), Failure> {
        let bytes = postcard::to_stdvec(record).map_err(|_| Failure::Operation {
            operation: Operation::Encode,
            kind: IoKind::InvalidData,
        })?;
        let dir = self.root.join(JOURNAL_DIR);
        let tmp = dir.join("active.tmp");
        write_sync(&tmp, &bytes)?;
        atomic_replace(&tmp, &self.journal_path())?;
        sync_dir(&dir)?;
        Ok(())
    }

    fn read_journal(&self) -> Result<Option<JournalRecord>, Failure> {
        match std::fs::read(self.journal_path()) {
            Ok(bytes) => postcard::from_bytes(&bytes)
                .map(Some)
                // An unreadable journal record is corruption we do not repair
                // heuristically.
                .map_err(|_| Failure::Integrity(Defect::CorruptJournal)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(Operation::Read, e)),
        }
    }

    fn remove_journal(&self) -> Result<(), Failure> {
        match std::fs::remove_file(self.journal_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(Operation::Remove, e)),
        }
        // Cleanup only: a lost removal is re-resolved by recovery.
        let _ = sync_dir(&self.root.join(JOURNAL_DIR));
        Ok(())
    }

    fn read_manifest_file(&self) -> Result<Option<(Manifest, [u8; 32])>, Failure> {
        match std::fs::read(self.root.join(MANIFEST_FILE)) {
            Ok(bytes) => {
                let manifest: Manifest = postcard::from_bytes(&bytes)
                    .map_err(|_| Failure::Integrity(Defect::CorruptManifest))?;
                if manifest.format_version != STORE_FORMAT_VERSION {
                    return Err(Failure::Integrity(Defect::UnsupportedFormat));
                }
                Ok(Some((manifest, manifest_hash(&bytes))))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(Operation::Read, e)),
        }
    }

    fn read_counter(&self) -> Result<u64, Failure> {
        match File::open(self.root.join(COUNTER_FILE)) {
            Ok(mut f) => {
                let mut buf = [0u8; 8];
                f.read_exact(&mut buf)
                    .map_err(|_| Failure::Integrity(Defect::MissingCounter))?;
                Ok(u64::from_le_bytes(buf))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A fresh store has no counter — but a store with a manifest
                // and no counter could reuse sequences: fail closed.
                if self.root.join(MANIFEST_FILE).exists() {
                    return Err(Failure::Integrity(Defect::MissingCounter));
                }
                Ok(0)
            }
            Err(e) => Err(io_err(Operation::Read, e)),
        }
    }

    fn reserve_sequence(&self) -> Result<u64, Failure> {
        let next = self
            .read_counter()?
            .checked_add(1)
            .ok_or(Failure::Integrity(Defect::CounterOverflow))?;
        let tmp = self.root.join(format!("{COUNTER_FILE}.tmp"));
        write_sync(&tmp, &next.to_le_bytes())?;
        atomic_replace(&tmp, &self.root.join(COUNTER_FILE))?;
        sync_dir(&self.root)?;
        Ok(next)
    }

    /// Crash recovery, then integrity verification, then orphan GC.
    fn recover(&mut self) -> Result<(), Failure> {
        match self.read_journal()? {
            None => {}
            Some(JournalRecord::Committed { .. }) => {
                // The commit fully landed; only the journal removal was lost.
                self.remove_journal()?;
            }
            Some(JournalRecord::Prepared { .. }) => {
                // Nothing was renamed yet: the old manifest is authoritative.
                // Orphan temps/objects are collected below.
                self.remove_journal()?;
            }
            Some(JournalRecord::MaterialReady {
                new_manifest_hash, ..
            }) => {
                let current = self.read_manifest_file()?;
                match current {
                    Some((_, hash)) if hash == new_manifest_hash => {
                        // The manifest swap completed: the new state must
                        // verify completely, then it is finalized as committed.
                        // (Verification happens below; a failure is an
                        // integrity error, not a heuristic repair.)
                        self.remove_journal()?;
                    }
                    _ => {
                        // The old manifest is still current: expose the old
                        // state; renamed-but-unreferenced objects are orphans.
                        self.remove_journal()?;
                    }
                }
            }
        }

        // Verify the exposed manifest, including the counter: a committed store
        // whose counter is missing or behind its manifest sequence could reuse
        // a sequence — fail closed.
        //
        // Verification proves the index structure whole — canonical encoding,
        // counts, prefix placement, shape — and then reads and re-hashes every
        // required object. That second half is deliberate and it is not cheap:
        // it is what makes "this store opened" mean "every object it promises
        // is present and is the bytes it claims", rather than deferring the
        // answer to whichever read happens to touch a corrupt object first.
        //
        // It is affordable only because the required set tracks live state.
        // Index nodes are kept by reachability rather than by requirement, so
        // the set does not grow with the number of commits ever performed;
        // `required_set_tracks_live_state` in `journal_faults` is what keeps
        // that true.
        if let Some((manifest, _)) = self.read_manifest_file()? {
            let source = ObjectNodes { root: &self.root };
            index::validate(&source, required_root(&manifest))
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
            for caller_root in &manifest.caller_index_roots {
                index::validate(&source, Some(child(*caller_root)))
                    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
            }
            let mut bad: Option<Defect> = None;
            index::stream(&source, required_root(&manifest), &mut |entry| {
                if bad.is_some() {
                    return;
                }
                let Some(len) = decode_len(&entry.value) else {
                    bad = Some(Defect::CorruptIndex);
                    return;
                };
                match std::fs::read(self.root.join(OBJECTS_DIR).join(hex(&entry.key))) {
                    Ok(bytes)
                        if u64::try_from(bytes.len()).ok() == Some(len)
                            && object_hash(&bytes) == entry.key => {}
                    Ok(_) => {
                        bad = Some(Defect::CorruptObject);
                    }
                    Err(_) => bad = Some(Defect::MissingObject),
                }
            })
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
            if let Some(defect) = bad {
                return Err(Failure::Integrity(defect));
            }
            if let Some(meta) = &manifest.caller_meta {
                self.read_object(meta)?;
            }
            let counter = self.read_counter()?;
            if counter < manifest.sequence {
                return Err(Failure::Integrity(Defect::MissingCounter));
            }
            self.manifest = Some(manifest);
        }

        self.sweep()?;
        let _ = std::fs::remove_file(self.root.join(format!("{COUNTER_FILE}.tmp")));
        let _ = std::fs::remove_file(self.root.join(format!("{MANIFEST_FILE}.tmp")));
        Ok(())
    }

    /// Collect every object no root reaches, without stopping the world.
    ///
    /// Recovery calls this at open, but a Station that stays up for weeks never
    /// reopens, so a session-long process would otherwise accumulate every
    /// superseded object it ever wrote. Collection is safe at any quiet moment
    /// because it is reachability-driven: an object is removed only when no
    /// index root reaches it and the required index does not name it, and a
    /// commit publishes its new root before anything it superseded stops being
    /// reachable.
    ///
    /// It costs one directory walk plus one lookup per candidate, so a caller
    /// runs it on an idle beat rather than inside a commit.
    pub fn collect_unreachable(&self) -> Result<(), Failure> {
        self.sweep()
    }

    /// Collect every object no root reaches. Streaming by construction: the
    /// index spine is held (one node per ~256 entries) and each candidate file
    /// is probed by lookup, so the complete required set is never rendered.
    fn sweep(&self) -> Result<(), Failure> {
        let Some(manifest) = self.manifest.clone() else {
            // No manifest: nothing is required, so everything is an orphan.
            if let Ok(entries) = std::fs::read_dir(self.root.join(OBJECTS_DIR)) {
                for entry in entries.flatten() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
            return Ok(());
        };
        let source = ObjectNodes { root: &self.root };
        let root = required_root(&manifest);
        let mut spine =
            index::spine(&source, root).map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        for caller_root in &manifest.caller_index_roots {
            spine.extend(
                index::spine(&source, Some(child(*caller_root)))
                    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?,
            );
        }

        let Ok(entries) = std::fs::read_dir(self.root.join(OBJECTS_DIR)) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
                continue;
            }
            let Some(hash) = unhex(&name) else {
                let _ = std::fs::remove_file(entry.path());
                continue;
            };
            if spine.contains(&hash) || manifest.caller_meta.is_some_and(|m| m.hash == hash) {
                continue;
            }
            let required = index::lookup(&source, root, &hash)
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
            if required.is_none() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// Whether the injector requests a crash at a **post-authoritative** point
    /// (where a crash may only lose cleanup, never the acknowledgment).
    fn crash_requested(&self, name: &str) -> bool {
        self.injector.as_ref().is_some_and(|i| i(name))
    }

    /// Commit against a caller's **complete** desired required set, computing
    /// the difference here.
    ///
    /// This is O(total required), which is exactly the cost [`Self::commit`]
    /// exists to avoid, so the choice between them is a real one. It is correct
    /// for a caller whose required set is small enough that enumerating it is
    /// not the cost that matters — the mechanics authority ledger, whose set is
    /// authority events, rather than a Replica's Body catalog, whose set is the
    /// thing that made a one-Body edit cost 28.8 MB. A caller with a large set
    /// must name its own delta.
    pub fn commit_required_set(
        &mut self,
        added: &[Vec<u8>],
        required: &[Object],
        meta: Vec<u8>,
    ) -> Result<u64, Failure> {
        // A named requirement must actually exist, or a "successful" commit
        // would fail integrity at the next open. The delta door cannot make this
        // mistake — it names only what changed — but this one takes a whole set
        // from a caller and has to check it.
        let arriving: std::collections::BTreeSet<[u8; 32]> =
            added.iter().map(|b| object_hash(b)).collect();
        for reference in required {
            if !arriving.contains(&reference.hash) {
                self.read_object(reference)?;
            }
        }
        let desired: std::collections::BTreeSet<[u8; 32]> = required
            .iter()
            .map(|r| r.hash)
            .chain(arriving.iter().copied())
            .collect();
        let mut removed = Vec::new();
        if let Some(manifest) = &self.manifest {
            let source = ObjectNodes { root: &self.root };
            index::stream(&source, required_root(manifest), &mut |entry| {
                if !desired.contains(&entry.key) {
                    removed.push(entry.key);
                }
            })
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        }
        // A caller that keeps its own index cannot use this door: the whole-set
        // diff above would drop every caller root it did not name, and this
        // signature has nowhere to name them.
        debug_assert!(
            self.manifest
                .as_ref()
                .is_none_or(|m| m.caller_index_roots.is_empty()),
            "commit_required_set would clear the caller index roots it cannot see"
        );
        self.commit(added, &removed, Index::NONE, meta)
    }

    /// Execute one journaled commit.
    ///
    /// `added` are new immutable objects, which become required. `removed`
    /// names object hashes whose requirement is dropped — the bytes survive
    /// until a sweep collects them, so a concurrent read cannot tear. `meta` is
    /// the caller's opaque metadata, stored as its own object.
    ///
    /// `caller_index` carries the caller's own authenticated index: its roots,
    /// and the nodes this commit produced. Those nodes are written but **not
    /// required** — like the store's own index nodes, they are kept alive by
    /// reachability from a root. That distinction is the whole lifecycle. A
    /// required entry is a promise that never expires, so an index node
    /// admitted as required would survive every rewrite that superseded it, and
    /// the store would grow with the number of commits it had ever performed
    /// rather than with what it holds.
    ///
    /// The commit writes only what changed: the added objects, the meta object,
    /// and the index nodes on the paths those changes touch. Nothing
    /// proportional to the store's size is encoded, cloned, or fsynced.
    ///
    /// **Acknowledgment discipline.** The manifest rename is the authoritative
    /// switch. Every failure *before* it leaves the old state exposed and
    /// returns an error; once the rename (and the store-directory sync that
    /// makes it power-loss durable) has succeeded, the commit **is** committed
    /// and this method returns `Ok` — journal cleanup failures after that point
    /// are absorbed, because recovery finalizes a `MaterialReady` journal with
    /// the new manifest as committed. A failure raised *by the directory sync
    /// itself* after the rename is the one genuinely ambiguous case and is
    /// reported as [`Failure::OutcomeUnknown`]: the caller must fail stop
    /// and reopen — recovery then resolves the outcome deterministically (the
    /// manifest on disk decides). A durably committed operation is therefore
    /// never reported as a plain retryable failure.
    pub fn commit(
        &mut self,
        added: &[Vec<u8>],
        removed: &[[u8; 32]],
        caller_index: Index<'_>,
        meta: Vec<u8>,
    ) -> Result<u64, Failure> {
        let caller_index_roots = caller_index.roots;
        // 1. Reserve the transaction counter (gaps allowed, reuse forbidden).
        self.point("counter")?;
        let sequence = self.reserve_sequence()?;

        let added_refs: Vec<Object> = added
            .iter()
            .map(|bytes| {
                Ok(Object {
                    hash: object_hash(bytes),
                    len: u64::try_from(bytes.len()).map_err(|_| Failure::Operation {
                        operation: Operation::Encode,
                        kind: IoKind::InvalidData,
                    })?,
                })
            })
            .collect::<Result<_, Failure>>()?;

        // Only nodes already on disk are read: `index::apply` decides whether a
        // subtree merges before it descends, so it never needs a node written
        // by this same commit.
        let source = ObjectNodes { root: &self.root };

        let mut changes: Vec<index::IndexChange> = added_refs
            .iter()
            .map(|r| index::IndexChange {
                key: r.hash,
                value: Some(encode_len(r.len)),
            })
            .collect();
        // A hash in both lists is the caller contradicting itself. The write
        // wins, because the bytes are about to be on disk and releasing them in
        // the same breath would make this commit's own objects collectable.
        let written_now: std::collections::BTreeSet<[u8; 32]> =
            added_refs.iter().map(|r| r.hash).collect();
        for hash in removed {
            if written_now.contains(hash) {
                continue;
            }
            changes.push(index::IndexChange {
                key: *hash,
                value: None,
            });
        }

        let meta_ref = Object {
            hash: object_hash(&meta),
            len: u64::try_from(meta.len()).map_err(|_| Failure::Operation {
                operation: Operation::Encode,
                kind: IoKind::InvalidData,
            })?,
        };

        let prior_root = self.manifest.as_ref().and_then(required_root);
        let mut sink = index::NodeSink::default();
        let new_root = index::apply(&source, prior_root, changes, &mut sink)
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;

        // Everything this commit must durably write: the caller's objects, the
        // metadata object, and the index nodes the update produced.
        let write_capacity = added
            .len()
            .checked_add(sink.written.len())
            .and_then(|total| total.checked_add(caller_index.nodes.len()))
            .and_then(|total| total.checked_add(1))
            .ok_or(Failure::Operation {
                operation: Operation::Encode,
                kind: IoKind::InvalidData,
            })?;
        let mut write_set: Vec<Vec<u8>> = Vec::with_capacity(write_capacity);
        write_set.extend_from_slice(added);
        write_set.push(meta);
        write_set.extend(sink.written);
        write_set.extend_from_slice(caller_index.nodes);
        let mut seen = std::collections::BTreeSet::new();
        write_set.retain(|bytes| seen.insert(object_hash(bytes)));

        let new_refs: Vec<Object> = write_set
            .iter()
            .map(|bytes| {
                Ok(Object {
                    hash: object_hash(bytes),
                    len: u64::try_from(bytes.len()).map_err(|_| Failure::Operation {
                        operation: Operation::Encode,
                        kind: IoKind::InvalidData,
                    })?,
                })
            })
            .collect::<Result<_, Failure>>()?;

        let manifest = Manifest {
            format_version: STORE_FORMAT_VERSION,
            sequence,
            required_object_index_root: new_root.map(coordinate),
            caller_meta: Some(meta_ref),
            caller_index_roots: caller_index_roots.to_vec(),
        };
        let manifest_bytes = postcard::to_stdvec(&manifest).map_err(|_| Failure::Operation {
            operation: Operation::Encode,
            kind: IoKind::InvalidData,
        })?;
        let new_manifest_hash = manifest_hash(&manifest_bytes);
        let new_objects: &[Vec<u8>] = &write_set;

        // 2. Journal Prepared.
        self.point("journal-prepared")?;
        self.write_journal(&JournalRecord::Prepared {
            sequence,
            new_objects: new_refs.clone(),
            new_manifest_hash,
        })?;

        // 3. Write all temporary objects.
        self.point("objects")?;
        for (obj, bytes) in new_refs.iter().zip(new_objects) {
            let tmp = self.object_path(&obj.hash).with_extension("tmp");
            write_sync(&tmp, bytes)?;
        }

        // 4. Journal MaterialReady, rename objects final, fsync their dir.
        self.point("journal-material-ready")?;
        self.write_journal(&JournalRecord::MaterialReady {
            sequence,
            new_objects: new_refs.clone(),
            new_manifest_hash,
        })?;
        self.point("rename-objects")?;
        for obj in &new_refs {
            let final_path = self.object_path(&obj.hash);
            if !final_path.exists() {
                atomic_replace(&final_path.with_extension("tmp"), &final_path)?;
            }
        }
        sync_dir(&self.root.join(OBJECTS_DIR))?;

        // 5. Manifest temp, then rename over current-manifest LAST.
        self.point("manifest-temp")?;
        let manifest_tmp = self.root.join(format!("{MANIFEST_FILE}.tmp"));
        write_sync(&manifest_tmp, &manifest_bytes)?;
        self.point("manifest-rename")?;
        atomic_replace(&manifest_tmp, &self.root.join(MANIFEST_FILE))?;
        if sync_dir(&self.root).is_err() {
            // The rename happened but its directory-entry durability is
            // unconfirmed: the one ambiguous outcome. Fail stop; reopening
            // resolves it (the on-disk manifest decides).
            return Err(Failure::OutcomeUnknown);
        }

        // --- The commit is now authoritative: nothing below may fail it. ---
        self.manifest = Some(manifest);

        // 6. Journal Committed + removal are pure cleanup: recovery finalizes a
        //    MaterialReady journal with the new manifest as committed, so a
        //    crash or error here loses nothing and MUST NOT fail the call.
        if !self.crash_requested("journal-committed") {
            let wrote = self
                .write_journal(&JournalRecord::Committed {
                    sequence,
                    new_manifest_hash,
                })
                .is_ok();
            if wrote && !self.crash_requested("journal-remove") {
                let _ = self.remove_journal();
            }
        }

        // 7. The periodic backstop. A commit orphans the index spine it
        //    rewrote, and a Station that stays up for weeks never reopens — so
        //    without this the object directory grows with the number of commits
        //    the session performed. Best-effort by construction: the commit is
        //    already authoritative, and the next open sweeps anyway.
        //
        //    Amortised rather than per-commit because a sweep walks the object
        //    directory once. A caller with an idle beat should pre-empt this by
        //    calling [`Self::collect_unreachable`] when nothing is waiting.
        self.commits_since_sweep = self.commits_since_sweep.saturating_add(1);
        if self.commits_since_sweep >= COMMITS_PER_SWEEP {
            self.commits_since_sweep = 0;
            let _ = self.sweep();
        }
        Ok(sequence)
    }
}
