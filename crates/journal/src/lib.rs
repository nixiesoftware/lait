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

//! The journaled durable store — the semantics-free commit protocol, served
//! by the pack log.
//!
//! This crate knows only immutable content-addressed objects, object
//! references, a manifest with opaque caller metadata sealed into every
//! commit, fault injection, and recovery. It knows nothing about Bodies,
//! authority, Worlds, or any product — both the Engine Body store and the
//! mechanics authority ledger commit through it.
//!
//! Physically, a store is a **pack**: an append-only slot family under the
//! store root (`hot-<generation>` files over a [`Medium`]), where a commit
//! appends its objects and one seal and flushes **once** — see [`pack`] for
//! the format and its crash discipline. There is no rename, no directory
//! sync, and no counter file on the commit path; the seal *is* the sequence
//! reservation, so a gap cannot exist and a reuse cannot be constructed. A
//! root still carrying the retired file-per-object layout is migrated at
//! open, once, verified end to end — see [`retired`] for the rules and the
//! tombstone an old binary meets afterwards.
//!
//! Recovery on open exposes **the complete old or the complete new** state:
//! the pack elects its newest fully-verified seal (a torn tail simply falls
//! off), and the semantic pass then re-validates both requirement indexes and
//! re-hashes every eager control object and the caller meta. Large protected
//! payloads live in a separately authenticated lazy-required index: open
//! never reads them — except inside the newest seal's delta, which recovery
//! crash-verifies whole because the physical layer cannot know classes — and
//! an exact Reader reports missing/corrupt material when it is requested.
//! Unreferenced objects are collected only by detached maintenance
//! (compaction into the next generation), never on the user/open path, and
//! Reader leases pin exact publication closures across a collection while
//! Readers themselves pin the generation they opened.
//!
//! **Required sets are indexes, not vectors.** A manifest used to carry
//! every required `Object` inline, so a commit re-encoded and fsynced the
//! whole list to change one object — 28.8 MB at 100,000 Bodies, measured in
//! `benchmarks/commit-cost-baseline.md`. It now carries a root hash into a
//! canonical radix index (see [`index`]), and a commit rewrites only the paths
//! its changed objects touch. The index's own nodes are objects too, kept alive
//! by reachability from the root rather than by being entries in it.
//!
//! Every commit boundary carries a named fault-injection point so the crash
//! matrix is testable; see [`Store::with_fault_injector`] and [`FAULT_POINTS`].

mod index;
mod medium;
#[cfg(test)]
mod migration_tests;
#[cfg(all(
    target_arch = "wasm32",
    not(target_feature = "atomics"),
    feature = "opfs"
))]
// The JS boundary is all f64: every cast here crosses it deliberately, and
// the remaining truncation lints stay visible as the workspace intends.
#[allow(clippy::as_conversions)]
mod opfs;
mod pack;
#[cfg(test)]
mod pack_tests;
#[allow(
    dead_code,
    reason = "the OPFS medium is this codec's only production caller"
)]
mod pool_header;
mod prior;
mod retired;

pub use medium::{DirMedium, Medium, MemMedium, ReadAt, SlotWriter};
#[cfg(all(
    target_arch = "wasm32",
    not(target_feature = "atomics"),
    feature = "opfs"
))]
pub use opfs::OpfsMedium;
#[cfg(any(test, feature = "fault-injection"))]
pub use pack::PACK_FAULT_POINTS;
pub use pack::{PackStore, PackView, Provenance};
pub use prior::Store as GenerationSource;

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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

const COUNTER_FILE: &str = "counter";
const MANIFEST_FILE: &str = "current-manifest";
const OBJECTS_DIR: &str = "objects";
const JOURNAL_DIR: &str = "journal";
const JOURNAL_FILE: &str = "active";
/// The slot family [`Store`] keeps its pack under.
const HOT_PREFIX: &str = "hot";

/// Domain for an object's content address.
const OBJECT_DOMAIN: &[u8] = b"lait/store-object/1";
/// Domain for a manifest's identity hash (referenced by the journal).
const MANIFEST_DOMAIN: &[u8] = b"lait/store-manifest/1";

#[cfg(any(test, feature = "fault-injection"))]
thread_local! {
    static WATCHED_RECOVERY_OBJECT: std::cell::RefCell<Option<([u8; 32], u64)>> =
        const { std::cell::RefCell::new(None) };
    static RECOVERY_INDEX_NODE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "fault-injection"))]
pub fn watch_recovery_object_reads(hash: [u8; 32]) {
    WATCHED_RECOVERY_OBJECT.with(|watched| *watched.borrow_mut() = Some((hash, 0)));
}

#[cfg(any(test, feature = "fault-injection"))]
pub fn watched_recovery_object_reads() -> u64 {
    WATCHED_RECOVERY_OBJECT.with(|watched| watched.borrow().map_or(0, |(_, reads)| reads))
}

#[cfg(any(test, feature = "fault-injection"))]
fn record_recovery_object_read(hash: [u8; 32]) {
    WATCHED_RECOVERY_OBJECT.with(|watched| {
        let mut watched = watched.borrow_mut();
        if let Some((target, reads)) = watched.as_mut() {
            if *target == hash {
                *reads = reads.saturating_add(1);
            }
        }
    });
}

#[cfg(any(test, feature = "fault-injection"))]
pub fn recovery_index_node_reads() -> u64 {
    RECOVERY_INDEX_NODE_READS.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "fault-injection"))]
fn reset_recovery_index_node_reads() {
    RECOVERY_INDEX_NODE_READS.with(|reads| reads.set(0));
}

#[cfg(any(test, feature = "fault-injection"))]
fn record_recovery_index_node_read() {
    RECOVERY_INDEX_NODE_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
}

#[cfg(not(any(test, feature = "fault-injection")))]
fn reset_recovery_index_node_reads() {}

#[cfg(not(any(test, feature = "fault-injection")))]
fn record_recovery_index_node_read() {}

#[cfg(not(any(test, feature = "fault-injection")))]
fn record_recovery_object_read(_hash: [u8; 32]) {}

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
    /// Two store generations disagree about history — a migrated source was
    /// mutated after its pack was sealed, or a sealed pack's source outlived
    /// it unexplained. Never resolved silently: only a person can say which
    /// side to keep.
    Diverged,
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
pub const STORE_FORMAT_VERSION: u8 = 4;

/// A delta to the authenticated lazy-required object class. Deferred objects
/// are written, fsynced, content-addressed, and protected from collection by
/// the same authoritative commit as eager objects, but `Store::open` validates
/// only their index coordinates. Their bytes are length/hash verified when a
/// Reader first asks for them.
#[derive(Debug, Clone, Copy)]
pub struct Deferred<'a> {
    pub added: &'a [Vec<u8>],
    pub removed: &'a [[u8; 32]],
}

impl Deferred<'_> {
    pub const NONE: Self = Self {
        added: &[],
        removed: &[],
    };
}

/// A caller's own authenticated index, as one commit sees it.
///
/// The two halves travel together because separating them corrupts silently:
/// roots without nodes name material the commit never wrote, and nodes without
/// roots are unreachable the moment they land.
#[derive(Debug, Clone, Copy)]
pub struct Index<'a> {
    /// The roots to record in the manifest. Replaces the previous set whole.
    pub roots: &'a [([u8; 32], u64)],
    /// Large caller indexes authenticated root-only at open. Exact lookup and
    /// detached scrub validate the reached path or complete tree.
    pub lazy_roots: &'a [([u8; 32], u64)],
    /// The nodes this commit produced. Written, never made required.
    pub nodes: &'a [Vec<u8>],
}

impl Index<'_> {
    /// For a caller that keeps no index of its own.
    pub const NONE: Self = Self {
        roots: &[],
        lazy_roots: &[],
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
    /// Root of the index mapping eagerly verified object hash to object
    /// length/class. Control, caller metadata, transactions, and other small
    /// correctness coordinates live here.
    /// `None` when nothing is required yet.
    pub eager_object_index_root: Option<([u8; 32], u64)>,
    /// Root of protected payloads whose hash/length verification is deferred
    /// until exact read. Open authenticates only this root node; exact lookup
    /// authenticates its path and detached scrub/GC validates the whole tree.
    /// Membership protects bytes from GC.
    pub deferred_object_index_root: Option<([u8; 32], u64)>,
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
    /// Caller-owned indexes whose entry population must not become startup
    /// work. Nodes remain GC-reachable; only recovery validation is path-lazy.
    pub lazy_caller_index_roots: Vec<([u8; 32], u64)>,
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

fn eager_root(manifest: &Manifest) -> Option<index::ChildRef> {
    manifest.eager_object_index_root.map(child)
}

fn deferred_root(manifest: &Manifest) -> Option<index::ChildRef> {
    manifest.deferred_object_index_root.map(child)
}

type FaultInjector = Box<dyn Fn(&str) -> bool + Send>;

/// The named fault points, in commit order (each fires before its
/// operation). Every commit failure before the flush leaves the old state
/// exposed and retryable; the flush is the authoritative switch, and nothing
/// follows it that can fail a commit.
#[cfg(any(test, feature = "fault-injection"))]
pub const FAULT_POINTS: [&str; 3] = ["pack-objects", "pack-seal", "pack-flush"];

/// The store engine: the semantic layer — authenticated requirement indexes,
/// caller roots, leases, reachability — served by the pack log.
///
/// The pack sits behind a mutex so detached maintenance can borrow the
/// writer from `&self`; reads never take it — they go through [`PackView`]
/// snapshots, which stay valid across commits and compactions.
pub struct Store {
    root: PathBuf,
    pack: Mutex<pack::PackStore>,
    manifest: Option<Manifest>,
    pins: Arc<Mutex<BTreeMap<[u8; 32], u64>>>,
    root_pins: Arc<Mutex<BTreeMap<([u8; 32], u64), u64>>>,
    /// Held until [`Store::release_owner_lock`] or the end of the process,
    /// however it ends.
    _lock: Option<StoreLock>,
}

/// Cloneable, immutable access to content-addressed journal objects.
///
/// A Reader captures a [`PackView`] — one generation and its table at one
/// seal — never the mutable Manifest. Callers take it after pinning their own
/// semantic index roots under the owning writer lock, then perform
/// potentially deep reads without holding that writer, across later commits
/// and compactions alike. Objects are verified by content address on every
/// read.
#[derive(Debug, Clone)]
pub struct Reader {
    view: pack::PackView,
    pins: Arc<Mutex<BTreeMap<[u8; 32], u64>>>,
    eager_root: Option<index::ChildRef>,
    deferred_root: Option<index::ChildRef>,
    _deferred_root_lease: Option<RootLease>,
}

/// A GC lease over exact content addresses. Clones share one lease; the final
/// drop releases every pin atomically from the collector's perspective.
#[derive(Debug, Clone)]
pub struct ObjectLease {
    _inner: Arc<PinnedObjects>,
}

/// An O(1) lease over one authenticated deferred-index root.
///
/// Exact publications keep this small coordinate rather than enumerating and
/// pinning every protected artifact they can reach. Detached GC validates and
/// traces every leased root before deleting anything.
#[derive(Debug, Clone)]
pub struct RootLease {
    _inner: Arc<PinnedRoot>,
}

#[derive(Debug)]
struct PinnedRoot {
    registry: Arc<Mutex<BTreeMap<([u8; 32], u64), u64>>>,
    root: ([u8; 32], u64),
}

impl Drop for PinnedRoot {
    fn drop(&mut self) {
        let Ok(mut pins) = self.registry.lock() else {
            return;
        };
        match pins.get_mut(&self.root) {
            Some(count) if *count > 1 => *count = count.saturating_sub(1),
            Some(_) => {
                pins.remove(&self.root);
            }
            None => {}
        }
    }
}

#[derive(Debug)]
struct PinnedObjects {
    registry: Arc<Mutex<BTreeMap<[u8; 32], u64>>>,
    hashes: Vec<[u8; 32]>,
}

impl Drop for PinnedObjects {
    fn drop(&mut self) {
        let Ok(mut pins) = self.registry.lock() else {
            return;
        };
        for hash in &self.hashes {
            match pins.get_mut(hash) {
                Some(count) if *count > 1 => *count = count.saturating_sub(1),
                Some(_) => {
                    pins.remove(hash);
                }
                None => {}
            }
        }
    }
}

// The pack's injector closure is not `Debug`; show the root + manifest.
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

/// One process per store. An OS advisory lock on a file beside the pack:
/// taken non-blocking at open, released by the kernel when the process ends
/// — crash included — so it can never go stale the way a pid file does.
#[derive(Debug)]
struct StoreLock {
    _file: std::fs::File,
}

impl StoreLock {
    const NAME: &'static str = "owner-lock";

    #[cfg(any(unix, windows))]
    fn acquire(root: &std::path::Path) -> Result<Self, Failure> {
        let refused = || Failure::Operation {
            operation: Operation::Open,
            kind: IoKind::PermissionDenied,
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(Self::NAME))
            .map_err(|e| io_err(Operation::Open, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: the descriptor is valid for the life of `file`.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                tracing::warn!(?root, "another process holds this store");
                return Err(refused());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
            };
            let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED =
                // SAFETY: OVERLAPPED is a plain C struct; zeroed is its
                // documented "no offset, no event" state.
                unsafe { std::mem::zeroed() };
            // SAFETY: the handle is valid for the life of `file` and the
            // OVERLAPPED outlives the call.
            let ok = unsafe {
                LockFileEx(
                    file.as_raw_handle() as _,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            };
            if ok == 0 {
                tracing::warn!(?root, "another process holds this store");
                return Err(refused());
            }
        }
        Ok(Self { _file: file })
    }
}

#[cfg(all(test, any(unix, windows)))]
mod owner_lock_tests {
    use super::*;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lait-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// One writer per root: a second open while the first stands is refused,
    /// not queued and not silently shared.
    #[test]
    fn a_second_open_is_refused_while_the_first_holds_custody() {
        let root = temp_root("held");
        let _first = Store::open(&root).unwrap();
        assert!(matches!(
            Store::open(&root),
            Err(Failure::Operation {
                operation: Operation::Open,
                kind: IoKind::PermissionDenied,
            })
        ));
    }

    /// Releasing custody is what lets a successor generation open in the same
    /// process the prior one is still draining in — a closed Station's core
    /// lingers in Session `Arc`s, and its frozen readers must not fence the
    /// next activation out of the store.
    #[test]
    fn released_custody_admits_a_successor_while_the_prior_store_stands() {
        let root = temp_root("released");
        let mut first = Store::open(&root).unwrap();
        first.release_owner_lock();
        let _second = Store::open(&root).unwrap();
    }

    /// The other half of the release-at-close claim: a prior generation's
    /// frozen reader keeps answering for the bytes it already verified while
    /// the successor commits past it — sealed bytes are immutable in place,
    /// so a coasting reader and a live writer never contradict each other.
    #[test]
    fn a_prior_readers_sealed_bytes_survive_the_successors_commits() {
        let root = temp_root("coast");
        let payload = b"sealed before handover".to_vec();
        let held = Object {
            hash: object_content_hash(&payload),
            len: payload.len() as u64,
        };
        let mut first = Store::open(&root).unwrap();
        first
            .commit(&[payload.clone()], &[], Index::NONE, b"m1".to_vec())
            .unwrap();
        let reader = first.reader();
        first.release_owner_lock();

        let mut successor = Store::open(&root).unwrap();
        successor
            .commit(
                &[b"the successor's material".to_vec()],
                &[],
                Index::NONE,
                b"m2".to_vec(),
            )
            .unwrap();

        assert_eq!(
            reader.read_object(&held).unwrap(),
            payload,
            "the coasting reader still answers for what it verified"
        );
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

/// The container generation immediately behind [`STORE_FORMAT_VERSION`]: one
/// required-object index rather than the eager/deferred split, and no lazy
/// caller indexes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorIndexedManifest {
    format_version: u8,
    sequence: u64,
    required_object_index_root: Option<([u8; 32], u64)>,
    caller_meta: Option<Object>,
    caller_index_roots: Vec<([u8; 32], u64)>,
}

/// The oldest container generation still opened here.
const PRIOR_STORE_FORMAT_VERSION: u8 = 2;

/// Decode a manifest, distinguishing a store this build cannot open from one
/// that is damaged.
///
/// The prior generation is recognized and refused rather than mapped forward.
/// Its manifest would map — the single required index is the eager one and the
/// indexes added since are empty-valid — but its index *values* carry no class
/// tag, so every leaf would have to be rewritten. That is a rebuild
/// (`GenerationSource` reads this generation), not an open, and an open that
/// mapped the root would write a current manifest over a prior index tree.
fn decode_manifest(bytes: &[u8]) -> Result<Manifest, Failure> {
    if let Ok(manifest) = postcard::from_bytes::<Manifest>(bytes) {
        if manifest.format_version == STORE_FORMAT_VERSION {
            return Ok(manifest);
        }
    }
    match postcard::from_bytes::<PriorIndexedManifest>(bytes) {
        Ok(prior) if prior.format_version == PRIOR_STORE_FORMAT_VERSION => {
            Err(Failure::Integrity(Defect::UnsupportedFormat))
        }
        _ => Err(Failure::Integrity(Defect::CorruptManifest)),
    }
}

#[cfg(test)]
mod manifest_generation_tests {
    use super::*;

    /// A store one generation behind is unsupported, not damaged. Reporting it
    /// as corrupt sends somebody looking for a defect that is not there — and
    /// the two call for opposite responses: a rebuild, or a restore.
    #[test]
    fn the_prior_generation_is_unsupported_and_only_nonsense_is_corrupt() {
        let prior = PriorIndexedManifest {
            format_version: PRIOR_STORE_FORMAT_VERSION,
            sequence: 12,
            required_object_index_root: None,
            caller_meta: None,
            caller_index_roots: Vec::new(),
        };
        let bytes = postcard::to_stdvec(&prior).expect("prior manifest");
        assert!(
            matches!(
                decode_manifest(&bytes),
                Err(Failure::Integrity(Defect::UnsupportedFormat))
            ),
            "the prior generation was not named as unsupported"
        );

        assert!(
            matches!(
                decode_manifest(&[0xff, 0xff, 0xff, 0xff]),
                Err(Failure::Integrity(Defect::CorruptManifest))
            ),
            "damage was not reported as damage"
        );
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequirementClass {
    Eager = 1,
    Deferred = 2,
}

impl RequirementClass {
    const fn tag(self) -> u8 {
        match self {
            Self::Eager => 1,
            Self::Deferred => 2,
        }
    }
}

/// An object's verification class and stored length, committed inside the
/// corresponding authenticated index leaf.
fn encode_requirement(class: RequirementClass, len: u64) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(9);
    encoded.push(class.tag());
    encoded.extend_from_slice(&len.to_be_bytes());
    encoded
}

fn decode_requirement(value: &[u8], expected: RequirementClass) -> Option<u64> {
    let (class, length) = value.split_first()?;
    if *class != expected.tag() {
        return None;
    }
    <[u8; 8]>::try_from(length).ok().map(u64::from_be_bytes)
}

fn requirement_length(
    source: &dyn index::NodeSource,
    eager: Option<index::ChildRef>,
    deferred: Option<index::ChildRef>,
    hash: &[u8; 32],
) -> Result<Option<u64>, Failure> {
    let eager_value = index::lookup_validated(source, eager, hash, &|value| {
        decode_requirement(value, RequirementClass::Eager).is_some()
    })
    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
    let deferred_value = index::lookup_validated(source, deferred, hash, &|value| {
        decode_requirement(value, RequirementClass::Deferred).is_some()
    })
    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
    match (eager_value, deferred_value) {
        (Some(_), Some(_)) => Err(Failure::Integrity(Defect::CorruptIndex)),
        (Some(value), None) => decode_requirement(&value, RequirementClass::Eager)
            .map(Some)
            .ok_or(Failure::Integrity(Defect::CorruptIndex)),
        (None, Some(value)) => decode_requirement(&value, RequirementClass::Deferred)
            .map(Some)
            .ok_or(Failure::Integrity(Defect::CorruptIndex)),
        (None, None) => Ok(None),
    }
}

fn deferred_requirement_length(
    source: &dyn index::NodeSource,
    deferred: Option<index::ChildRef>,
    hash: &[u8; 32],
) -> Result<Option<u64>, Failure> {
    index::lookup_validated(source, deferred, hash, &|value| {
        decode_requirement(value, RequirementClass::Deferred).is_some()
    })
    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?
    .map(|value| {
        decode_requirement(&value, RequirementClass::Deferred)
            .ok_or(Failure::Integrity(Defect::CorruptIndex))
    })
    .transpose()
}

/// Read one object out of a pack view under the caller's admitted bound,
/// keeping the old file-read error vocabulary exactly: absent is
/// [`Defect::MissingObject`], a length that disagrees with the expectation or
/// bytes that disagree with the address are [`Defect::CorruptObject`].
fn read_view_bounded(
    view: &pack::PackView,
    hash: &[u8; 32],
    expected_len: u64,
    max_len: u64,
) -> Result<Vec<u8>, Failure> {
    if expected_len > max_len {
        return Err(Failure::Integrity(Defect::CorruptObject));
    }
    match view.object_len(hash) {
        None => {
            tracing::warn!(object = %hex(hash), "journal object is absent");
            Err(Failure::Integrity(Defect::MissingObject))
        }
        Some(stored_len) if stored_len != expected_len => {
            Err(Failure::Integrity(Defect::CorruptObject))
        }
        Some(_) => view.read_bounded(hash, max_len),
    }
}

/// Reads index nodes out of a pack view. Index nodes are ordinary
/// content-addressed objects; what makes them nodes is that a root reaches
/// them.
struct PackNodes<'a> {
    view: &'a pack::PackView,
}

impl index::NodeSource for PackNodes<'_> {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        // The view verifies the content address on every read.
        self.view.read(hash).ok()
    }
}

struct RecoveryNodes<'a>(PackNodes<'a>);

impl index::NodeSource for RecoveryNodes<'_> {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        record_recovery_index_node_read();
        self.0.node(hash)
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

impl Store {
    /// Open a store root, running crash recovery, and return the store plus
    /// its current manifest (`None` for a fresh store). The exposed state is
    /// always the complete old or complete new one. A root still carrying
    /// the retired file-per-object layout is migrated here, once, verified
    /// end to end — see [`retired`] for the rules — and a root another process
    /// already holds is refused.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Failure> {
        Self::open_root(root.into(), None)
    }

    /// Open with the injector armed before anything runs — the only way a
    /// test can crash a migration, which happens inside open.
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn open_with_fault_injector(
        root: impl Into<PathBuf>,
        injector: Box<dyn Fn(&str) -> bool + Send>,
    ) -> Result<Self, Failure> {
        Self::open_root(root.into(), Some(injector))
    }

    fn open_root(root: PathBuf, injector: Option<FaultInjector>) -> Result<Self, Failure> {
        let medium = medium::DirMedium::open(&root).map_err(|e| io_err(Operation::Open, e))?;
        #[cfg(any(unix, windows))]
        let lock = Some(StoreLock::acquire(&root)?);
        #[cfg(not(any(unix, windows)))]
        let lock = None;
        Self::open_native(Arc::new(medium), root, lock, injector)
    }

    /// Open over any medium — the browser path, where the medium is OPFS or
    /// memory. No owner lock (the platform's exclusive handle is the lock)
    /// and no migration: no prior-layout store can exist where the prior
    /// layout never ran.
    pub fn open_on(medium: Arc<dyn Medium>) -> Result<Self, Failure> {
        let pack = pack::PackStore::open(medium, HOT_PREFIX)?;
        Self::assemble(PathBuf::new(), pack, None)
    }

    /// Hand write custody of this root to a successor while frozen readers
    /// coast. The lock exists to keep two WRITERS off one pack; a closed
    /// store that will never commit again holds only readers, and sealed
    /// bytes are immutable in place (a successor appends past `sealed_len`
    /// and never truncates below it), so releasing early is what lets a new
    /// generation open in the same process the old one is still draining in.
    pub fn release_owner_lock(&mut self) {
        self._lock = None;
    }

    fn open_native(
        medium: Arc<dyn Medium>,
        root: PathBuf,
        lock: Option<StoreLock>,
        injector: Option<FaultInjector>,
    ) -> Result<Self, Failure> {
        let mut pack = match pack::PackStore::open(medium.clone(), HOT_PREFIX) {
            Ok(pack) => pack,
            // A pack that cannot open beside an authoritative, untombstoned
            // prior-layout source is a stillborn migration — a crash tore
            // the seal before retirement ever began. The source still rules:
            // prove it stands on its own, clear the stillborn slots, and let
            // the ordinary flow migrate again. Refusing here would brick a
            // store whose data is entirely intact.
            Err(Failure::Integrity(defect))
                if retired::present(&root) && !retired::tombstoned(&root) =>
            {
                tracing::warn!(
                    ?root,
                    ?defect,
                    "unusable pack beside an authoritative prior-layout source; re-migrating"
                );
                retired::Source::open(&root).map(drop)?;
                pack::remove_family(medium.as_ref(), HOT_PREFIX)?;
                pack::PackStore::open(medium.clone(), HOT_PREFIX)?
            }
            Err(failure) => return Err(failure),
        };
        #[cfg(any(test, feature = "fault-injection"))]
        if let Some(injector) = injector {
            pack.set_fault_injector(injector);
        }
        #[cfg(not(any(test, feature = "fault-injection")))]
        let _ = injector;
        if pack.manifest().is_none() {
            if retired::tombstoned(&root) {
                // The tombstone promises a pack that is not here: somebody
                // removed the slots, or restored half a backup. Only a person
                // can say which side of history to keep.
                return Err(Failure::Integrity(Defect::Diverged));
            }
            if retired::present(&root) {
                migrate_retired(&root, &mut pack)?;
            }
        } else if retired::present(&root) {
            // A sealed pack beside prior-layout remnants: a crash between
            // the migration seal and the end of retirement. Only a source
            // that still matches the seal's provenance may be retired.
            resume_retirement(&root, &pack)?;
        }
        Self::assemble(root, pack, lock)
    }

    fn assemble(
        root: PathBuf,
        pack: pack::PackStore,
        lock: Option<StoreLock>,
    ) -> Result<Self, Failure> {
        let manifest = verify_semantics(&pack)?;
        Ok(Self {
            root,
            pack: Mutex::new(pack),
            manifest,
            pins: Arc::new(Mutex::new(BTreeMap::new())),
            root_pins: Arc::new(Mutex::new(BTreeMap::new())),
            _lock: lock,
        })
    }

    fn pack(&self) -> Result<std::sync::MutexGuard<'_, pack::PackStore>, Failure> {
        self.pack.lock().map_err(|_| Failure::Operation {
            operation: Operation::Open,
            kind: IoKind::Other,
        })
    }

    fn view(&self) -> Result<pack::PackView, Failure> {
        Ok(self.pack()?.view())
    }

    /// Where one object's payload lives — slot name, offset, length — so a
    /// corruption test can reach bytes while the store is closed, and restore
    /// them even after the damage makes the pack refuse to open.
    #[cfg(any(test, feature = "fault-injection"))]
    #[must_use]
    pub fn object_location(&self, hash: &[u8; 32]) -> Option<(String, u64, u64)> {
        self.pack.lock().ok()?.object_location(hash)
    }

    /// Attach a fault injector (test seam; see [`FAULT_POINTS`]).
    #[must_use]
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn with_fault_injector(self, injector: Box<dyn Fn(&str) -> bool + Send>) -> Self {
        self.set_fault_injector(injector);
        self
    }

    /// Attach a fault injector by reference (test seam for callers embedding
    /// the store; see [`FAULT_POINTS`]).
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn set_fault_injector(&self, injector: Box<dyn Fn(&str) -> bool + Send>) {
        if let Ok(mut pack) = self.pack.lock() {
            pack.set_fault_injector(injector);
        }
    }

    /// Pin a read-only object handle. Semantic roots must be captured by the
    /// caller at the same commit point; this handle deliberately cannot read
    /// or mutate the live Manifest.
    pub fn reader(&self) -> Reader {
        let deferred_root = self.manifest.as_ref().and_then(deferred_root);
        let deferred_root_lease = deferred_root.map(|root| {
            let coordinate = coordinate(root);
            if let Ok(mut pins) = self.root_pins.lock() {
                let count = pins.entry(coordinate).or_insert(0);
                *count = count.saturating_add(1);
            }
            RootLease {
                _inner: Arc::new(PinnedRoot {
                    registry: self.root_pins.clone(),
                    root: coordinate,
                }),
            }
        });
        Reader {
            // A poisoned pack lock surfaces on the writer's path; a reader
            // minted after that still deserves a coherent snapshot, so fall
            // back to the poisoned guard's view rather than panic.
            view: match self.pack.lock() {
                Ok(pack) => pack.view(),
                Err(held) => held.into_inner().view(),
            },
            pins: self.pins.clone(),
            eager_root: self.manifest.as_ref().and_then(eager_root),
            deferred_root,
            _deferred_root_lease: deferred_root_lease,
        }
    }

    /// The current manifest, if any commit has completed.
    #[must_use]
    pub const fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// How many objects the pack physically holds — required or not, index
    /// nodes included. The growth diagnostic: after a sweep this is the live
    /// population, between sweeps it includes what commits superseded.
    pub fn stored_objects(&self) -> Result<usize, Failure> {
        Ok(self.view()?.object_count())
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
        let view = self.view()?;
        let source = PackNodes { view: &view };
        let mut out = Vec::new();
        for (root, class) in [
            (eager_root(manifest), RequirementClass::Eager),
            (deferred_root(manifest), RequirementClass::Deferred),
        ] {
            index::stream(&source, root, &mut |entry| {
                if let Some(len) = decode_requirement(&entry.value, class) {
                    out.push(Object {
                        hash: entry.key,
                        len,
                    });
                }
            })
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        }
        out.sort_by_key(|object| object.hash);
        out.dedup_by_key(|object| object.hash);
        Ok(out)
    }

    /// Whether one object is currently required. O(index depth).
    pub fn is_required(&self, hash: &[u8; 32]) -> Result<bool, Failure> {
        let Some(manifest) = &self.manifest else {
            return Ok(false);
        };
        let view = self.view()?;
        let source = PackNodes { view: &view };
        requirement_length(&source, eager_root(manifest), deferred_root(manifest), hash)
            .map(|length| length.is_some())
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
        let view = self.view()?;
        if let Some(manifest) = &self.manifest {
            if let Some(committed_len) = requirement_length(
                &PackNodes { view: &view },
                eager_root(manifest),
                deferred_root(manifest),
                &obj.hash,
            )? {
                if committed_len != obj.len {
                    return Err(Failure::Integrity(Defect::CorruptObject));
                }
            }
        }
        read_view_bounded(&view, &obj.hash, obj.len, obj.len)
    }

    /// Read an immutable object without allocating above the caller's already
    /// admitted bound. The pack table is checked before allocation; length
    /// and content address are checked before the bytes are believed.
    pub fn read_object_bounded(&self, obj: &Object, max_len: u64) -> Result<Vec<u8>, Failure> {
        read_view_bounded(&self.view()?, &obj.hash, obj.len, max_len)
    }

    /// Read an object that must be present in the authenticated eager/deferred
    /// requirement index, checking its committed length before allocation.
    pub fn read_required_object_bounded(
        &self,
        obj: &Object,
        max_len: u64,
    ) -> Result<Vec<u8>, Failure> {
        let Some(manifest) = &self.manifest else {
            return Err(Failure::Integrity(Defect::MissingObject));
        };
        let view = self.view()?;
        match requirement_length(
            &PackNodes { view: &view },
            eager_root(manifest),
            deferred_root(manifest),
            &obj.hash,
        )? {
            Some(committed_len) if committed_len == obj.len => {}
            Some(_) => return Err(Failure::Integrity(Defect::CorruptObject)),
            None => return Err(Failure::Integrity(Defect::MissingObject)),
        }
        read_view_bounded(&view, &obj.hash, obj.len, max_len)
    }

    /// Read a payload committed specifically to the deferred requirement
    /// class. Unlike the general required lookup, this never touches the eager
    /// index; an old root lease therefore needs to pin only its deferred tree.
    pub fn read_deferred_object_bounded(
        &self,
        obj: &Object,
        max_len: u64,
    ) -> Result<Vec<u8>, Failure> {
        let Some(manifest) = &self.manifest else {
            return Err(Failure::Integrity(Defect::MissingObject));
        };
        let view = self.view()?;
        match deferred_requirement_length(
            &PackNodes { view: &view },
            deferred_root(manifest),
            &obj.hash,
        )? {
            Some(committed_len) if committed_len == obj.len => {}
            Some(_) => return Err(Failure::Integrity(Defect::CorruptObject)),
            None => return Err(Failure::Integrity(Defect::MissingObject)),
        }
        read_view_bounded(&view, &obj.hash, obj.len, max_len)
    }

    /// Read one immutable object by its content address.
    pub fn read(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Failure> {
        self.view()?.read_bounded(
            hash,
            u64::try_from(index::MAX_NODE_BYTES).unwrap_or(u64::MAX),
        )
    }

    /// Collect every object no root reaches, without stopping the world.
    ///
    /// Runtime calls this as detached, governor-admitted maintenance.
    /// Collection is safe because an object survives whenever a caller index,
    /// eager/deferred requirement, or live Reader lease names it — and
    /// because collection is a **compaction**: the live set is copied,
    /// re-verified, into the next pack generation, and the old generation
    /// stays readable for every Reader that predates the copy.
    ///
    /// The cost moved with the format: this rewrites the live bytes rather
    /// than probing and unlinking, so a caller runs it when garbage has
    /// accumulated, not on every idle beat.
    pub fn collect_unreachable(&self) -> Result<(), Failure> {
        self.sweep()
    }

    /// Compact to the live set. The reachability question is answered above
    /// the physical layer, exactly as it always was: validated indexes,
    /// spines, pins, leased deferred roots, caller meta.
    fn sweep(&self) -> Result<(), Failure> {
        let view = self.view()?;
        let Some(manifest) = self.manifest.clone() else {
            // No manifest: nothing is required, so only pins keep anything.
            let pinned = self
                .pins
                .lock()
                .map(|pins| pins.clone())
                .unwrap_or_default();
            let mut pack = self.pack()?;
            pack.compact(&|hash| pinned.contains_key(hash))?;
            return Ok(());
        };
        let source = PackNodes { view: &view };
        let eager = eager_root(&manifest);
        let deferred = deferred_root(&manifest);
        let leased_deferred: Vec<index::ChildRef> = self
            .root_pins
            .lock()
            .map(|pins| {
                pins.keys()
                    .copied()
                    .map(child)
                    .filter(|root| Some(*root) != deferred)
                    .collect()
            })
            .unwrap_or_default();
        // A collector may delete only after a complete authenticated scrub.
        // A corrupt unopened deferred leaf is repairable/unavailable, never a
        // reason to guess that the bytes it used to protect are unreachable.
        index::validate(&source, eager).map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        index::validate(&source, deferred).map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        for root in &leased_deferred {
            index::validate(&source, Some(*root))
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        }
        for caller_root in &manifest.caller_index_roots {
            index::validate(&source, Some(child(*caller_root)))
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        }
        for caller_root in &manifest.lazy_caller_index_roots {
            index::validate(&source, Some(child(*caller_root)))
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        }
        let mut spine =
            index::spine(&source, eager).map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        spine.extend(
            index::spine(&source, deferred)
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?,
        );
        for root in &leased_deferred {
            spine.extend(
                index::spine(&source, Some(*root))
                    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?,
            );
        }
        for caller_root in &manifest.caller_index_roots {
            spine.extend(
                index::spine(&source, Some(child(*caller_root)))
                    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?,
            );
        }
        for caller_root in &manifest.lazy_caller_index_roots {
            spine.extend(
                index::spine(&source, Some(child(*caller_root)))
                    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?,
            );
        }

        let pinned: BTreeMap<[u8; 32], u64> = self
            .pins
            .lock()
            .map(|pins| pins.clone())
            .unwrap_or_default();
        // The closure reads through the snapshot taken above, never through
        // the pack writer it is deciding for; an index failure mid-decision
        // keeps the object, because a collector may only delete what a
        // complete authenticated lookup released.
        let live = |hash: &[u8; 32]| -> bool {
            if pinned.contains_key(hash)
                || spine.contains(hash)
                || manifest.caller_meta.is_some_and(|m| m.hash == *hash)
            {
                return true;
            }
            let required = index::lookup(&source, eager, hash).unwrap_or(Some(Vec::new()));
            let lazy_required = index::lookup(&source, deferred, hash).unwrap_or(Some(Vec::new()));
            let leased_required = leased_deferred.iter().any(|root| {
                index::lookup(&source, Some(*root), hash)
                    .unwrap_or(Some(Vec::new()))
                    .is_some()
            });
            required.is_some() || lazy_required.is_some() || leased_required
        };
        self.pack()?.compact(&live)
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
            let view = self.view()?;
            let source = PackNodes { view: &view };
            index::stream(&source, eager_root(manifest), &mut |entry| {
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
            self.manifest.as_ref().is_none_or(|m| {
                m.caller_index_roots.is_empty()
                    && m.lazy_caller_index_roots.is_empty()
                    && m.deferred_object_index_root.is_none()
            }),
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
    /// **Acknowledgment discipline.** The pack's single flush is the
    /// authoritative switch. Every failure *before* it leaves the old state
    /// exposed, returns a retryable error, and discards the partial tail;
    /// nothing follows the flush that can fail a commit. A failure raised
    /// *by the flush itself* is the one genuinely ambiguous case and is
    /// reported as [`Failure::OutcomeUnknown`]: the writer is poisoned and
    /// answers the same until the caller fail-stops and reopens — recovery
    /// then resolves the outcome deterministically (the newest seal that
    /// verifies decides). A durably committed operation is therefore never
    /// reported as a plain retryable failure.
    pub fn commit(
        &mut self,
        added: &[Vec<u8>],
        removed: &[[u8; 32]],
        caller_index: Index<'_>,
        meta: Vec<u8>,
    ) -> Result<u64, Failure> {
        self.commit_classified(added, removed, Deferred::NONE, caller_index, meta)
    }

    /// Execute one journaled commit with a separately authenticated
    /// lazy-required payload delta. Bytes in both classes ride the identical
    /// append/seal/flush path; only recovery verification policy differs.
    pub fn commit_classified(
        &mut self,
        eager_added: &[Vec<u8>],
        eager_removed: &[[u8; 32]],
        deferred: Deferred<'_>,
        caller_index: Index<'_>,
        meta: Vec<u8>,
    ) -> Result<u64, Failure> {
        let caller_index_roots = caller_index.roots;
        let lazy_caller_index_roots = caller_index.lazy_roots;
        // 1. The next sequence is the pack's last sealed one plus one: the
        //    seal is the reservation, so a failed commit leaves no gap and a
        //    reuse is structurally impossible.
        let sequence = self
            .pack()?
            .sequence()
            .checked_add(1)
            .ok_or(Failure::Integrity(Defect::CounterOverflow))?;

        let eager_refs: Vec<Object> = eager_added
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
        let eager_hashes: std::collections::BTreeSet<[u8; 32]> =
            eager_refs.iter().map(|reference| reference.hash).collect();
        let deferred_refs: Vec<Object> = deferred
            .added
            .iter()
            .filter(|bytes| !eager_hashes.contains(&object_hash(bytes)))
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

        // Only sealed nodes are read: `index::apply` decides whether a
        // subtree merges before it descends, so it never needs a node written
        // by this same commit — which is exactly what a view can never see.
        let view = self.view()?;
        let source = PackNodes { view: &view };

        let mut eager_changes: Vec<index::IndexChange> = eager_refs
            .iter()
            .map(|r| index::IndexChange {
                key: r.hash,
                value: Some(encode_requirement(RequirementClass::Eager, r.len)),
            })
            .collect();
        // A hash in both lists is the caller contradicting itself. The write
        // wins, because the bytes are about to be on disk and releasing them in
        // the same breath would make this commit's own objects collectable.
        for hash in eager_removed {
            if eager_hashes.contains(hash) {
                continue;
            }
            eager_changes.push(index::IndexChange {
                key: *hash,
                value: None,
            });
        }
        let deferred_hashes: std::collections::BTreeSet<[u8; 32]> = deferred_refs
            .iter()
            .map(|reference| reference.hash)
            .collect();
        // Reclassification is atomic: a hash can inhabit exactly one class.
        eager_changes.extend(deferred_hashes.iter().map(|hash| index::IndexChange {
            key: *hash,
            value: None,
        }));
        let mut deferred_changes: Vec<index::IndexChange> = deferred_refs
            .iter()
            .map(|reference| index::IndexChange {
                key: reference.hash,
                value: Some(encode_requirement(
                    RequirementClass::Deferred,
                    reference.len,
                )),
            })
            .collect();
        for hash in deferred.removed {
            if deferred_hashes.contains(hash) {
                continue;
            }
            deferred_changes.push(index::IndexChange {
                key: *hash,
                value: None,
            });
        }
        deferred_changes.extend(eager_hashes.iter().map(|hash| index::IndexChange {
            key: *hash,
            value: None,
        }));

        let meta_ref = Object {
            hash: object_hash(&meta),
            len: u64::try_from(meta.len()).map_err(|_| Failure::Operation {
                operation: Operation::Encode,
                kind: IoKind::InvalidData,
            })?,
        };

        let prior_eager_root = self.manifest.as_ref().and_then(eager_root);
        let prior_deferred_root = self.manifest.as_ref().and_then(deferred_root);
        let mut sink = index::NodeSink::default();
        let new_eager_root = index::apply(&source, prior_eager_root, eager_changes, &mut sink)
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        let new_deferred_root =
            index::apply(&source, prior_deferred_root, deferred_changes, &mut sink)
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;

        // Everything this commit must durably write: the caller's objects, the
        // metadata object, and the index nodes the update produced.
        let write_capacity = eager_added
            .len()
            .checked_add(deferred.added.len())
            .and_then(|total| total.checked_add(sink.written.len()))
            .and_then(|total| total.checked_add(caller_index.nodes.len()))
            .and_then(|total| total.checked_add(1))
            .ok_or(Failure::Operation {
                operation: Operation::Encode,
                kind: IoKind::InvalidData,
            })?;
        let mut write_set: Vec<Vec<u8>> = Vec::with_capacity(write_capacity);
        write_set.extend_from_slice(eager_added);
        write_set.extend_from_slice(deferred.added);
        write_set.push(meta);
        write_set.extend(sink.written);
        write_set.extend_from_slice(caller_index.nodes);
        let mut seen = std::collections::BTreeSet::new();
        write_set.retain(|bytes| seen.insert(object_hash(bytes)));

        let manifest = Manifest {
            format_version: STORE_FORMAT_VERSION,
            sequence,
            eager_object_index_root: new_eager_root.map(coordinate),
            deferred_object_index_root: new_deferred_root.map(coordinate),
            caller_meta: Some(meta_ref),
            caller_index_roots: caller_index_roots.to_vec(),
            lazy_caller_index_roots: lazy_caller_index_roots.to_vec(),
        };
        let manifest_bytes = postcard::to_stdvec(&manifest).map_err(|_| Failure::Operation {
            operation: Operation::Encode,
            kind: IoKind::InvalidData,
        })?;

        // 2. One pack commit: objects, seal, one flush. The flush is the
        //    authoritative switch; every failure before it leaves the old
        //    state exposed and retryable, and a flush failure is
        //    OutcomeUnknown — the pack poisons its writer, and reopening
        //    resolves the outcome deterministically from what verifies.
        let committed = self.pack()?.commit(&write_set, manifest_bytes)?;
        debug_assert_eq!(
            committed, sequence,
            "the manifest's sequence and the seal's must be one number"
        );

        // --- The commit is now authoritative: nothing below may fail it. ---
        self.manifest = Some(manifest);
        Ok(sequence)
    }
}

impl Reader {
    /// Pin exact object addresses against detached GC. Pinning performs no
    /// object read and does not make an unrequired object semantically live;
    /// callers capture these refs with their own exact publication root.
    pub fn pin_objects(&self, objects: &[Object]) -> ObjectLease {
        let mut hashes: Vec<[u8; 32]> = objects.iter().map(|object| object.hash).collect();
        hashes.sort();
        hashes.dedup();
        if let Ok(mut pins) = self.pins.lock() {
            for hash in &hashes {
                let count = pins.entry(*hash).or_insert(0);
                *count = count.saturating_add(1);
            }
        }
        ObjectLease {
            _inner: Arc::new(PinnedObjects {
                registry: self.pins.clone(),
                hashes,
            }),
        }
    }

    /// Read one immutable object by content address.
    pub fn read(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Failure> {
        self.view.read_bounded(
            hash,
            u64::try_from(index::MAX_NODE_BYTES).unwrap_or(u64::MAX),
        )
    }

    /// Read and length-check an immutable object.
    pub fn read_object(&self, object: &Object) -> Result<Vec<u8>, Failure> {
        if let Some(committed_len) = requirement_length(
            &PackNodes { view: &self.view },
            self.eager_root,
            self.deferred_root,
            &object.hash,
        )? {
            if committed_len != object.len {
                return Err(Failure::Integrity(Defect::CorruptObject));
            }
        }
        self.read_object_bounded(object, object.len)
    }

    /// Read and verify one object without allocating above `max_len`.
    pub fn read_object_bounded(&self, object: &Object, max_len: u64) -> Result<Vec<u8>, Failure> {
        read_view_bounded(&self.view, &object.hash, object.len, max_len)
    }

    /// Read a required object, validating authenticated class/length before
    /// the bounded allocation.
    pub fn read_required_object_bounded(
        &self,
        object: &Object,
        max_len: u64,
    ) -> Result<Vec<u8>, Failure> {
        match requirement_length(
            &PackNodes { view: &self.view },
            self.eager_root,
            self.deferred_root,
            &object.hash,
        )? {
            Some(committed_len) if committed_len == object.len => {}
            Some(_) => return Err(Failure::Integrity(Defect::CorruptObject)),
            None => return Err(Failure::Integrity(Defect::MissingObject)),
        }
        self.read_object_bounded(object, max_len)
    }

    /// Read an exact deferred payload under this pinned deferred root only.
    pub fn read_deferred_object_bounded(
        &self,
        object: &Object,
        max_len: u64,
    ) -> Result<Vec<u8>, Failure> {
        match deferred_requirement_length(
            &PackNodes { view: &self.view },
            self.deferred_root,
            &object.hash,
        )? {
            Some(committed_len) if committed_len == object.len => {}
            Some(_) => return Err(Failure::Integrity(Defect::CorruptObject)),
            None => return Err(Failure::Integrity(Defect::MissingObject)),
        }
        self.read_object_bounded(object, max_len)
    }
}

/// Decode and verify everything the pack's sealed manifest claims — the
/// semantic half of recovery, run at every open. Verification proves both
/// requirement indexes whole, re-reads and re-hashes every eager control
/// object and the caller meta, and refuses a manifest whose sequence has
/// outrun the seal that carries it. Deferred payloads are deliberately not
/// touched: exact Readers verify them on demand, so a million-record Station
/// open never becomes a full payload scan.
fn verify_semantics(pack: &pack::PackStore) -> Result<Option<Manifest>, Failure> {
    let Some(bytes) = pack.manifest() else {
        return Ok(None);
    };
    let manifest = decode_manifest(bytes)?;
    reset_recovery_index_node_reads();
    let view = pack.view();
    let source = RecoveryNodes(PackNodes { view: &view });
    index::validate(&source, eager_root(&manifest))
        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
    index::validate_root(&source, deferred_root(&manifest))
        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
    for caller_root in &manifest.caller_index_roots {
        index::validate(&source, Some(child(*caller_root)))
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
    }
    for caller_root in &manifest.lazy_caller_index_roots {
        index::validate_root(&source, Some(child(*caller_root)))
            .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
    }
    let mut bad: Option<Defect> = None;
    index::stream(&source, eager_root(&manifest), &mut |entry| {
        if bad.is_some() {
            return;
        }
        let Some(len) = decode_requirement(&entry.value, RequirementClass::Eager) else {
            bad = Some(Defect::CorruptIndex);
            return;
        };
        record_recovery_object_read(entry.key);
        match read_view_bounded(&view, &entry.key, len, len) {
            Ok(_) => {}
            Err(Failure::Integrity(defect)) => bad = Some(defect),
            Err(_) => bad = Some(Defect::MissingObject),
        }
    })
    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
    if let Some(defect) = bad {
        return Err(Failure::Integrity(defect));
    }
    if let Some(meta) = &manifest.caller_meta {
        read_view_bounded(&view, &meta.hash, meta.len, meta.len)?;
    }
    if manifest.sequence > pack.sequence() {
        return Err(Failure::Integrity(Defect::MissingCounter));
    }
    Ok(Some(manifest))
}

/// Carry an old-layout store into the pack: verify the source whole, stream
/// every object through a re-hashing read into one migration seal, make the
/// slot's existence durable with the one directory fsync the pack medium
/// never performs, then retire the source. Every step before the seal leaves
/// the source authoritative; every step after is resumable cleanup.
fn migrate_retired(root: &std::path::Path, pack: &mut pack::PackStore) -> Result<(), Failure> {
    let source = retired::Source::open(root)?;
    if source.manifest_bytes.is_empty() {
        // The old layout was opened but never committed: nothing to carry,
        // and nothing whose sequence anyone could have observed.
        tracing::info!(?root, "retiring a never-committed prior-layout store");
        retired::retire(root)?;
        return Ok(());
    }
    let needed = source.total_bytes();
    if let Some(available) = retired::available_bytes(root) {
        let margin = (needed / 10).saturating_add(64 * 1024 * 1024);
        if available < needed.saturating_add(margin) {
            tracing::warn!(
                ?root,
                needed,
                available,
                "migration refused: not enough space to copy the store; the \
                 old layout remains authoritative"
            );
            return Err(Failure::Operation {
                operation: Operation::Write,
                kind: IoKind::Other,
            });
        }
    }
    let provenance = pack::Provenance {
        source_manifest: source.manifest_hash(),
        source_counter: source.counter,
    };
    tracing::info!(
        ?root,
        objects = source.objects.len(),
        counter = source.counter,
        "migrating the prior-layout store into the pack"
    );
    // Rot in an orphan the exposed state never promised — leftovers of some
    // crashed old commit — is skippable history; rot in anything required
    // fail-stops, exactly as validation already enforces for the eager set.
    let mut stream = source
        .objects
        .iter()
        .filter_map(|hash| match source.read_object(hash) {
            Ok(bytes) => Some(Ok(bytes)),
            Err(Failure::Integrity(Defect::CorruptObject)) => match source.is_required(hash) {
                Ok(false) => {
                    tracing::warn!(object = %hex(hash), "skipping a rotted unrequired object");
                    None
                }
                _ => Some(Err(Failure::Integrity(Defect::CorruptObject))),
            },
            Err(failure) => Some(Err(failure)),
        });
    pack.migrate_commit(&mut stream, source.manifest_bytes.clone(), provenance)?;
    // The pack medium never syncs directories; the migration driver must,
    // or a crash could lose the slot file after the source is retired.
    retired::sync_dir(root)?;
    retired::retire(root)
}

/// Finish a retirement a crash interrupted — but only for a source the
/// pack's own seal vouches for. A source manifest that no longer matches the
/// recorded provenance was written to *after* migration; deciding which
/// history wins is not this code's call.
fn resume_retirement(root: &std::path::Path, pack: &pack::PackStore) -> Result<(), Failure> {
    let Some(provenance) = pack.provenance() else {
        tracing::warn!(
            ?root,
            "prior-layout remnants beside a pack with no provenance"
        );
        return Err(Failure::Integrity(Defect::Diverged));
    };
    // The crash may have landed before migration's own directory sync: the
    // slot's existence must be durable before the source starts moving, on
    // this path exactly as on the first attempt.
    retired::sync_dir(root)?;
    match std::fs::read(root.join(MANIFEST_FILE)) {
        Ok(bytes) if !retired::is_tombstone(&bytes) => {
            if manifest_hash(&bytes) != provenance.source_manifest {
                tracing::warn!(
                    ?root,
                    "the migrated source changed after its pack was sealed"
                );
                return Err(Failure::Integrity(Defect::Diverged));
            }
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(Operation::Read, e)),
    }
    retired::retire(root)
}

/// Flip bytes of one stored object in place — the corruption a rot test
/// needs, aimed through the pack's own table because objects no longer have
/// a file of their own to tamper with. Answers whether anything was flipped.
#[cfg(any(test, feature = "fault-injection"))]
pub fn corrupt_object_for_test(root: &std::path::Path, hash: &[u8; 32]) -> bool {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    let Ok(medium) = medium::DirMedium::open(root) else {
        return false;
    };
    let Ok(pack) = pack::PackStore::open(Arc::new(medium), HOT_PREFIX) else {
        return false;
    };
    let Some((slot, offset, len)) = pack.object_location(hash) else {
        return false;
    };
    drop(pack);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(slot))
    else {
        return false;
    };
    let take = usize::try_from(len.min(8)).unwrap_or(8);
    let mut bytes = vec![0u8; take];
    if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut bytes).is_err() {
        return false;
    }
    for byte in &mut bytes {
        *byte ^= 0xFF;
    }
    file.seek(SeekFrom::Start(offset)).is_ok() && file.write_all(&bytes).is_ok()
}
