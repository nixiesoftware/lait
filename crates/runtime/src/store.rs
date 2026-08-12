//! The Orbit's durable on-disk footprint and its exclusive lock.
//!
//! An Orbit lives under `<root>/<space-id>/`. This module owns three of that
//! store's files: the private Replica `marker` (what Space this is,
//! and that it is a Replica store at all), an `epoch` counter durably
//! incremented before each activation, and a `lock` file carrying the OS
//! advisory exclusive lock that is the typed double-lock — only one
//! operational owner at a time. The Engine journaled store's files (`counter`,
//! `current-manifest`, `objects/`, `journal/`) live alongside these in the
//! same directory; the two touch disjoint names.
//!
//! Technical file/lock terms are correct at this layer — it is below the domain
//! boundary.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use mechanics::ids::SpaceId;
use serde::{Deserialize, Serialize};

use crate::lifecycle::{Failure, Integrity, Persistence};

const MARKER_FILE: &str = "marker";
const EPOCH_FILE: &str = "epoch";
const LOCK_FILE: &str = "lock";
const STORE_MAGIC: &[u8] = b"lait/replica/1";
const STORE_VERSION: u8 = 1;
const MAX_MARKER: usize = 4 * 1024;
const SPACE_ID_LEN: usize = 29;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MarkerBody {
    space: [u8; SPACE_ID_LEN],
    checksum: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreMarker {
    version: u8,
    space: [u8; SPACE_ID_LEN],
    checksum: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkerInvalid {
    NotReplica,
    UnsupportedVersion,
    Corrupt,
}

fn marker_checksum(version: u8, space: &[u8; SPACE_ID_LEN]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(STORE_MAGIC);
    hash.update(&[version]);
    hash.update(space);
    *hash.finalize().as_bytes()
}

impl StoreMarker {
    fn new(space: &SpaceId) -> Option<Self> {
        let space = <[u8; SPACE_ID_LEN]>::try_from(space.as_str().as_bytes()).ok()?;
        Some(Self {
            version: STORE_VERSION,
            space,
            checksum: marker_checksum(STORE_VERSION, &space),
        })
    }

    fn encode(&self) -> Vec<u8> {
        #[allow(
            clippy::expect_used,
            reason = "derived serialization of the fixed store marker is infallible"
        )]
        let body = postcard::to_stdvec(&MarkerBody {
            space: self.space,
            checksum: self.checksum,
        })
        .expect("postcard marker body");
        let mut encoded = Vec::with_capacity(
            STORE_MAGIC
                .len()
                .saturating_add(1)
                .saturating_add(body.len()),
        );
        encoded.extend_from_slice(STORE_MAGIC);
        encoded.push(self.version);
        encoded.extend_from_slice(&body);
        encoded
    }

    fn classify(bytes: &[u8]) -> Result<Self, MarkerInvalid> {
        if bytes.len() > MAX_MARKER {
            return Err(MarkerInvalid::Corrupt);
        }
        let prefix_len = STORE_MAGIC.len().saturating_add(1);
        if bytes.len() < prefix_len || bytes.get(..STORE_MAGIC.len()) != Some(STORE_MAGIC) {
            return Err(MarkerInvalid::NotReplica);
        }
        let version = *bytes.get(STORE_MAGIC.len()).ok_or(MarkerInvalid::Corrupt)?;
        if version != STORE_VERSION {
            return Err(MarkerInvalid::UnsupportedVersion);
        }
        let body: MarkerBody =
            postcard::from_bytes(bytes.get(prefix_len..).ok_or(MarkerInvalid::Corrupt)?)
                .map_err(|_| MarkerInvalid::Corrupt)?;
        if body.checksum != marker_checksum(version, &body.space) {
            return Err(MarkerInvalid::Corrupt);
        }
        Ok(Self {
            version,
            space: body.space,
            checksum: body.checksum,
        })
    }

    fn space(&self) -> Option<SpaceId> {
        std::str::from_utf8(&self.space)
            .ok()
            .and_then(SpaceId::parse)
    }
}

fn io_err(e: std::io::Error) -> Failure {
    Failure::Persistence(Persistence::Io(e.kind()))
}

/// A test seam mirroring the Engine journal's: called with a named fault point
/// *before* the named operation executes; returning `true` makes the operation
/// fail there, modelling a crash or an I/O failure.
type StoreFaultInjector = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The named store fault points, in epoch-bump order.
const STORE_FAULT_POINTS: [&str; 3] = ["epoch-temp", "epoch-rename", "epoch-dir-sync"];

/// A handle to an Orbit's store directory.
#[derive(Clone)]
pub struct OrbitStore {
    dir: PathBuf,
    space: SpaceId,
    injector: Option<StoreFaultInjector>,
}

impl std::fmt::Debug for OrbitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrbitStore")
            .field("dir", &self.dir)
            .field("space", &self.space)
            .finish_non_exhaustive()
    }
}

impl OrbitStore {
    fn dir_for(root: &Path, space: &SpaceId) -> PathBuf {
        root.join(space.as_str())
    }

    /// Form a fresh store for `space`: create the directory, write the marker,
    /// and initialize the epoch counter to zero. Fails if a store already
    /// exists there.
    pub fn create(root: &Path, space: &SpaceId) -> Result<Self, Failure> {
        let dir = Self::dir_for(root, space);
        if dir.join(MARKER_FILE).exists() {
            return Err(Failure::AlreadyExists(space.clone()));
        }
        std::fs::create_dir_all(&dir).map_err(io_err)?;
        let marker = StoreMarker::new(space).ok_or(Failure::Integrity(Integrity::SpaceIdentity))?;
        write_sync(&dir.join(MARKER_FILE), &marker.encode())?;
        write_sync(&dir.join(EPOCH_FILE), &0u64.to_le_bytes())?;
        // Make the new directory entries themselves durable — a formation whose
        // directory entries could vanish on power loss must not report success.
        sync_dir(&dir).map_err(io_err)?;
        sync_dir(root).map_err(io_err)?;
        Ok(Self {
            dir,
            space: space.clone(),
            injector: None,
        })
    }

    /// Open an existing store, validating its marker against `space`.
    pub fn open(root: &Path, space: &SpaceId) -> Result<Self, Failure> {
        let dir = Self::dir_for(root, space);
        let marker_bytes = match std::fs::read(dir.join(MARKER_FILE)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Failure::OrbitNotFound(space.clone()))
            }
            Err(e) => return Err(io_err(e)),
        };
        let marker = StoreMarker::classify(&marker_bytes).map_err(marker_err)?;
        if marker.space().as_ref() != Some(space) {
            return Err(Failure::Integrity(Integrity::Marker));
        }
        Ok(Self {
            dir,
            space: space.clone(),
            injector: None,
        })
    }

    /// Attach a fault injector (test seam; see [`STORE_FAULT_POINTS`]).
    fn with_fault_injector(mut self, injector: StoreFaultInjector) -> Self {
        self.injector = Some(injector);
        self
    }

    fn point(&self, name: &str) -> Result<(), Failure> {
        if let Some(injector) = &self.injector {
            if injector(name) {
                return Err(Failure::Persistence(Persistence::InjectedFault));
            }
        }
        Ok(())
    }

    pub fn space(&self) -> &SpaceId {
        &self.space
    }

    /// The current durable epoch (zero if never activated).
    pub fn read_epoch(&self) -> Result<u64, Failure> {
        // `create` writes the epoch and every later write is an atomic replace,
        // so a missing or short epoch file is corruption — never "zero". Reading
        // it as zero would reuse committed epochs, which activation must never
        // do; fail closed instead.
        let mut f = match File::open(self.dir.join(EPOCH_FILE)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Failure::Integrity(Integrity::Epoch))
            }
            Err(e) => return Err(io_err(e)),
        };
        let mut buf = [0u8; 8];
        f.read_exact(&mut buf)
            .map_err(|_| Failure::Integrity(Integrity::Epoch))?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Atomically increment the epoch, returning the new value. The new value is
    /// written to a temp sibling, fsynced, and atomically renamed over the epoch
    /// file — a crash at any point leaves either the complete old or the
    /// complete new value, never a partial one. Every phase **including the
    /// directory synchronization** is fallible and fault-injected: activation
    /// must not report success while durable epoch establishment is unknown,
    /// because Beacon freshness depends on never reusing an epoch a live
    /// Station acted under. A failure aborts activation; the un-acknowledged
    /// epoch was never used, so re-deriving it later is safe.
    pub fn bump_epoch(&self) -> Result<u64, Failure> {
        let next = self
            .read_epoch()?
            .checked_add(1)
            .ok_or(Failure::EpochOverflow)?;
        let tmp = self.dir.join(format!("{EPOCH_FILE}.tmp"));
        self.point("epoch-temp")?;
        write_sync(&tmp, &next.to_le_bytes())?;
        self.point("epoch-rename")?;
        atomic_replace(&tmp, &self.dir.join(EPOCH_FILE)).map_err(io_err)?;
        self.point("epoch-dir-sync")?;
        sync_dir(&self.dir).map_err(io_err)?;
        Ok(next)
    }

    /// Acquire the exclusive store lock (the operational-ownership / double-lock
    /// guard). Returns [`Failure::ReplicaLocked`] if another owner holds
    /// it.
    pub fn acquire_lock(&self) -> Result<StoreLock, Failure> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join(LOCK_FILE))
            .map_err(io_err)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(StoreLock { file: Some(file) }),
            Err(_) => Err(Failure::ReplicaLocked(self.space.clone())),
        }
    }

    /// Whether the store is currently locked by some operational owner, tested
    /// non-destructively (advisory; used by observation).
    pub fn is_locked(&self) -> bool {
        match OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join(LOCK_FILE))
        {
            Ok(file) => match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = FileExt::unlock(&file);
                    false
                }
                Err(_) => true,
            },
            // If we cannot even open the lock file, treat as not lockable-by-us.
            Err(_) => true,
        }
    }

    /// The stable Orbit directory. Identity, lifecycle, neighbor, and cache
    /// material live here across every derived generation.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Bytes on disk under this Orbit's store directory.
    ///
    /// Attributable to one Space by construction — the store is one directory
    /// per Space — and deliberately the *whole* directory: the Replica's
    /// journal, the mechanics ledger beside it, the content cache, every
    /// superseded generation, and this module's own marker/epoch/lock. Those
    /// are all bytes this Space is occupying on somebody's disk, and a figure
    /// that quietly dropped the superseded generations would under-report the
    /// thing a person opens a storage surface to find out.
    ///
    /// **It is a walk, and there is no cheaper honest answer.** The journal
    /// keeps every object as its own file, so no component holds a total.
    /// Summing the lengths the required-object index records is cheaper and is
    /// a *different number wearing this one's name*: it omits the journal
    /// itself, the cache, the mechanics ledger, prior generations, and every
    /// object a sweep has not collected yet. Dressing that up as "bytes on
    /// disk" is precisely the synthesised figure a storage surface must never
    /// draw.
    ///
    /// One `stat` per file, so nothing on a commit, Contact or placement path
    /// calls it — only an explicit storage read reaches this.
    ///
    /// A read failure fails the **whole** measurement rather than returning the
    /// partial sum, so the caller reports the footprint absent. An undercount
    /// presented as a measurement is worse than no measurement.
    pub fn footprint_bytes(&self) -> Result<u64, Failure> {
        let mut total: u64 = 0;
        let mut pending = vec![self.dir.clone()];
        // The root is the one directory that has to be there. If it is gone
        // there is nothing to measure, and answering zero would draw a store
        // that vanished as a store that is empty — so its read error
        // propagates while a subdirectory disappearing mid-walk does not.
        let mut root = true;
        while let Some(dir) = pending.pop() {
            let entries = match (std::fs::read_dir(&dir), root) {
                (Ok(entries), _) => entries,
                (Err(e), false) if e.kind() == std::io::ErrorKind::NotFound => continue,
                (Err(e), _) => return Err(io_err(e)),
            };
            root = false;
            for entry in entries {
                let entry = entry.map_err(io_err)?;
                // `DirEntry::metadata` does not follow symlinks, so a link
                // counts as the link: a store can neither be walked out of nor
                // made to report bytes that live somewhere else.
                let meta = match entry.metadata() {
                    Ok(meta) => meta,
                    // Collected between the listing and the stat. A file that
                    // no longer exists occupies nothing, so skipping it is the
                    // measurement rather than a hole in it.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(io_err(e)),
                };
                if meta.is_dir() {
                    pending.push(entry.path());
                } else {
                    total = total.saturating_add(meta.len());
                }
            }
        }
        Ok(total)
    }

    /// The Replica component selected by the Orbit's one generation pointer.
    pub fn replica_dir(&self) -> Result<PathBuf, Failure> {
        crate::generation::Active::read(&self.dir)
            .map(|active| active.path(crate::generation::Component::Replica))
            .map_err(|_| Failure::Integrity(Integrity::Replica))
    }

    /// Destroy the store directory. The caller must hold the lock (i.e. be the
    /// operational owner) so a live Station's store is never removed underneath
    /// it.
    pub fn remove(&self) -> Result<(), Failure> {
        std::fs::remove_dir_all(&self.dir).map_err(io_err)
    }

    /// Every Space with a valid store marker under `root`.
    pub fn list(root: &Path) -> Vec<SpaceId> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return out;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            if let Ok(bytes) = std::fs::read(entry.path().join(MARKER_FILE)) {
                if let Ok(marker) = StoreMarker::classify(&bytes) {
                    if let Some(space) = marker.space() {
                        out.push(space);
                    }
                }
            }
        }
        out.sort();
        out
    }
}

/// The held exclusive lock. Dropping it (or calling [`StoreLock::release`])
/// releases the OS lock — this is how "the lock is released last" is enforced:
/// the Station holds this, and it outlives every tracked task by construction.
#[derive(Debug)]
pub struct StoreLock {
    file: Option<File>,
}

impl StoreLock {
    /// Explicitly release the lock now.
    pub fn release(mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
        }
    }
}

fn write_sync(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(io_err)?;
    f.write_all(bytes).map_err(io_err)?;
    f.sync_all().map_err(io_err)?;
    Ok(())
}

/// Atomically move `tmp` over `dst`, replacing any existing file. `std::fs::
/// rename` replaces on both platforms lait targets (Windows uses `MoveFileExW`
/// with `MOVEFILE_REPLACE_EXISTING`), but on Windows a transient sharing
/// violation (antivirus/indexer holding the destination) can fail a single
/// attempt — retry briefly before giving up.
fn atomic_replace(tmp: &Path, dst: &Path) -> std::io::Result<()> {
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
    last.map_or_else(|| Ok(()), Err)
}

/// Directory durability after a rename/create, so the directory entry itself
/// survives a crash. On unix this is a real fsync of the directory, and a
/// failure fails the calling phase. On Windows a directory handle needs
/// `FILE_FLAG_BACKUP_SEMANTICS` to open; if no handle can be opened at all the
/// platform does not expose directory sync and NTFS's metadata journaling is
/// the documented durability contract — but a handle that opens and then fails
/// to flush is a real error and fails the phase. (The same contract as the
/// Engine journal's directory sync.)
#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir).and_then(|d| d.sync_all())
}

#[cfg(windows)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
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
        Ok(d) => d.sync_all(),
    }
}

fn marker_err(e: MarkerInvalid) -> Failure {
    match e {
        MarkerInvalid::NotReplica | MarkerInvalid::UnsupportedVersion | MarkerInvalid::Corrupt => {
            Failure::Integrity(Integrity::Marker)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lait-store-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn marker_of_version(version: u8) -> Vec<u8> {
        let current = StoreMarker::new(&SpaceId::from_digest([31u8; 16])).unwrap();
        let mut bytes = current.encode();
        bytes[STORE_MAGIC.len()] = version;
        bytes
    }

    #[test]
    fn marker_format_and_classification_remain_stable() {
        assert_eq!(STORE_MAGIC, b"lait/replica/1");
        let space = SpaceId::from_digest([31u8; 16]);
        let marker = StoreMarker::new(&space).unwrap();
        assert_eq!(StoreMarker::classify(&marker.encode()).unwrap(), marker);
        assert_eq!(marker.space(), Some(space));

        assert_eq!(
            StoreMarker::classify(b"not lait at all"),
            Err(MarkerInvalid::NotReplica)
        );
        assert_eq!(
            StoreMarker::classify(&marker_of_version(STORE_VERSION + 1)),
            Err(MarkerInvalid::UnsupportedVersion)
        );

        let mut corrupt = marker.encode();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        assert_eq!(StoreMarker::classify(&corrupt), Err(MarkerInvalid::Corrupt));
        assert_eq!(
            StoreMarker::classify(&vec![0; MAX_MARKER + 1]),
            Err(MarkerInvalid::Corrupt)
        );
    }

    #[test]
    fn marker_version_is_refused_before_its_body_is_parsed() {
        let mut bytes = STORE_MAGIC.to_vec();
        bytes.push(STORE_VERSION + 1);
        bytes.extend_from_slice(b"not a postcard marker body");
        assert_eq!(
            StoreMarker::classify(&bytes),
            Err(MarkerInvalid::UnsupportedVersion)
        );
    }

    #[test]
    fn an_epoch_fault_at_every_point_aborts_without_acknowledging() {
        // Durable epoch establishment must be all-or-nothing from the caller's
        // view: a fault at ANY bump phase — including the directory sync —
        // fails the bump, and the durable epoch remains readable as either the
        // complete old or the complete new value (never acknowledged-but-lost).
        for &point in STORE_FAULT_POINTS.iter() {
            let root = temp_root();
            let space = SpaceId::from_digest([7u8; 16]);
            let store = OrbitStore::create(&root, &space).unwrap();
            assert_eq!(store.read_epoch().unwrap(), 0);
            let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let armed2 = armed.clone();
            let faulty = store.clone().with_fault_injector(Arc::new(move |name| {
                name == point && armed2.load(Ordering::SeqCst)
            }));
            let err = faulty.bump_epoch().unwrap_err();
            assert!(
                matches!(err, Failure::Persistence(Persistence::InjectedFault)),
                "fault at {point} must abort the bump"
            );
            // The store is intact: the epoch reads as a complete value and the
            // next (un-faulted) bump succeeds and never reuses an acknowledged
            // epoch.
            armed.store(false, Ordering::SeqCst);
            let read = store.read_epoch().unwrap();
            assert!(read == 0 || read == 1, "complete old or complete new");
            let next = store.bump_epoch().unwrap();
            assert!(next > read, "the bump advances past whatever was durable");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_footprint_counts_every_byte_under_the_store_and_nothing_beside_it() {
        let root = temp_root();
        let space = SpaceId::from_digest([9u8; 16]);
        let store = OrbitStore::create(&root, &space).unwrap();

        // A formed store is already occupying disk — the marker and the epoch
        // counter are files. Zero would be wrong here, which is why the
        // baseline is asserted rather than assumed.
        let baseline = store.footprint_bytes().unwrap();
        assert!(baseline > 0, "marker + epoch are bytes on disk");

        // Nested, because the journal keeps its objects in a subdirectory and a
        // walk that stopped at the top level would silently report a fraction.
        let nested = store.dir().join("objects").join("deeper");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("object"), vec![7u8; 4_096]).unwrap();
        assert_eq!(
            store.footprint_bytes().unwrap(),
            baseline + 4_096,
            "the walk must reach every level of the store"
        );

        // Another Space's store is another Space's footprint. The figure is
        // attributable to one Space or it is not worth drawing.
        let other = OrbitStore::create(&root, &SpaceId::from_digest([10u8; 16])).unwrap();
        std::fs::write(other.dir().join("bulk"), vec![1u8; 65_536]).unwrap();
        assert_eq!(
            store.footprint_bytes().unwrap(),
            baseline + 4_096,
            "a neighbouring store's bytes are not this one's"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_store_that_is_gone_reports_no_footprint_rather_than_a_zero() {
        // The defect this forbids: a removed or unreadable store measuring as
        // `0 B` and drawing as an empty Space. A failed measurement has to
        // surface as a failure so the caller can report it absent.
        let root = temp_root();
        let space = SpaceId::from_digest([11u8; 16]);
        let store = OrbitStore::create(&root, &space).unwrap();
        store.remove().unwrap();
        assert!(
            store.footprint_bytes().is_err(),
            "an absent store is not an empty one"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn consecutive_epoch_bumps_are_monotone() {
        let root = temp_root();
        let space = SpaceId::from_digest([8u8; 16]);
        let store = OrbitStore::create(&root, &space).unwrap();
        let mut last = 0;
        for _ in 0..10 {
            let next = store.bump_epoch().unwrap();
            assert_eq!(next, last + 1);
            last = next;
        }
        assert_eq!(store.read_epoch().unwrap(), 10);
        let _ = std::fs::remove_dir_all(&root);
    }
}
