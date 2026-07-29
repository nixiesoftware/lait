//! The resident cache — bytes that are *here*, as opposed to bytes that are
//! *required*.
//!
//! The journal's object store makes one promise: everything a root names is
//! present and intact, and a missing object is an integrity failure that is
//! never repaired heuristically. That promise is exactly wrong for content
//! chunks. A chunk is optional by design — a peer may hold none of a gigabyte
//! it can name — and losing one should mean "fetch it again", not "this store
//! is broken".
//!
//! So residency is a separate directory with a separate API, and the separation
//! is the safety property: a caller cannot satisfy a required-object reference
//! from an unverified cache entry, because the two are not the same call.
//!
//! Like the rest of this crate it is semantics-free. It stores blobs addressed
//! by hash, each with an optional sidecar blob beside it, and named tags that
//! keep entries alive. It does not know what content is, what a proof proves,
//! or why anything is pinned.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::{atomic_replace, io_err, sync_dir, write_sync, JournalError};

const CHUNKS_DIR: &str = "chunks";
const SIDECARS_DIR: &str = "sidecars";
const TAGS_DIR: &str = "tags";
const STAGING_DIR: &str = "staging";

/// Why a cache operation failed.
///
/// Deliberately distinct from [`JournalError`]: a missing resident entry is
/// **not** an integrity failure. Collapsing the two is what would let a
/// refetchable chunk take a store down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// The entry is not here. Expected, and usually means "go fetch it".
    NotResident,
    /// The bytes are here but do not match the address they are filed under, or
    /// their sidecar is missing. The entry is dropped and becomes refetchable.
    Corrupt,
    /// A durable write failed.
    Durability(String),
    /// A bound was exceeded before anything was allocated.
    Bounds,
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::NotResident => write!(f, "not resident"),
            CacheError::Corrupt => write!(f, "corrupt cache entry"),
            CacheError::Durability(m) => write!(f, "durability: {m}"),
            CacheError::Bounds => write!(f, "bounds"),
        }
    }
}
impl std::error::Error for CacheError {}

impl From<JournalError> for CacheError {
    fn from(e: JournalError) -> Self {
        CacheError::Durability(e.to_string())
    }
}

/// A hold on one entry, keyed by the operation that took it.
///
/// Keyed per *operation*, not per entry: two concurrent fetches of the same
/// content are two leases over one chunk set, and the bytes survive until the
/// last of them releases. A lease keyed by content id alone would let the first
/// transfer to finish sweep the second's staged bytes.
///
/// The name is derived rather than stored, so an interrupted operation's holds
/// are recoverable after restart without a side file to keep consistent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lease {
    pub operation: [u8; 16],
    pub entry: [u8; 32],
}

impl Lease {
    fn tag_name(&self) -> String {
        format!(
            "{}.{}",
            data_encoding::HEXLOWER.encode(&self.operation),
            data_encoding::HEXLOWER.encode(&self.entry)
        )
    }

    fn parse(name: &str) -> Option<Self> {
        let (op, entry) = name.split_once('.')?;
        let op = data_encoding::HEXLOWER.decode(op.as_bytes()).ok()?;
        let entry = data_encoding::HEXLOWER.decode(entry.as_bytes()).ok()?;
        Some(Self {
            operation: <[u8; 16]>::try_from(op.as_slice()).ok()?,
            entry: <[u8; 32]>::try_from(entry.as_slice()).ok()?,
        })
    }
}

/// A durable pin: a caller's statement that an entry should survive quota
/// pressure. Unlike a lease it has no operation and no expiry.
fn pin_name(entry: &[u8; 32]) -> String {
    format!("pin.{}", data_encoding::HEXLOWER.encode(entry))
}

/// What a sweep did, so a caller can report rather than guess.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub entries_removed: u64,
    pub bytes_reclaimed: u64,
    pub staging_removed: u64,
}

/// The resident cache.
pub struct ResidentCache {
    root: PathBuf,
    /// Maximum resident bytes before a sweep starts evicting. Operator policy.
    quota_bytes: u64,
}

fn hex(hash: &[u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(hash)
}

fn unhex(name: &str) -> Option<[u8; 32]> {
    let raw = data_encoding::HEXLOWER.decode(name.as_bytes()).ok()?;
    <[u8; 32]>::try_from(raw.as_slice()).ok()
}

impl ResidentCache {
    /// Open (creating) a cache under `root`, reclaiming anything a previous run
    /// left half-installed.
    pub fn open(root: impl Into<PathBuf>, quota_bytes: u64) -> Result<Self, CacheError> {
        let root = root.into();
        for dir in [CHUNKS_DIR, SIDECARS_DIR, TAGS_DIR, STAGING_DIR] {
            std::fs::create_dir_all(root.join(dir))
                .map_err(|e| CacheError::Durability(format!("cache dir: {e}")))?;
        }
        let cache = Self { root, quota_bytes };
        cache.reclaim_incomplete()?;
        Ok(cache)
    }

    pub fn quota_bytes(&self) -> u64 {
        self.quota_bytes
    }

    fn chunk_path(&self, entry: &[u8; 32]) -> PathBuf {
        self.root.join(CHUNKS_DIR).join(hex(entry))
    }

    fn sidecar_path(&self, entry: &[u8; 32]) -> PathBuf {
        self.root.join(SIDECARS_DIR).join(hex(entry))
    }

    /// Install one verified entry: its bytes and the sidecar that proves them.
    ///
    /// Ordering is the contract. Both temporaries are written and fsynced, the
    /// bytes are checked against the address they will be filed under, then
    /// both are renamed. An interruption anywhere leaves temporaries, which the
    /// next open reclaims — never a chunk without its sidecar, which would be
    /// an entry that cannot be served and cannot be told apart from one that
    /// can.
    pub fn install(
        &self,
        entry: &[u8; 32],
        bytes: &[u8],
        sidecar: &[u8],
    ) -> Result<(), CacheError> {
        if crate::object_content_hash(bytes) != *entry {
            return Err(CacheError::Corrupt);
        }
        let chunk_tmp = self.chunk_path(entry).with_extension("tmp");
        let sidecar_tmp = self.sidecar_path(entry).with_extension("tmp");
        write_sync(&chunk_tmp, bytes)?;
        write_sync(&sidecar_tmp, sidecar)?;
        atomic_replace(&sidecar_tmp, &self.sidecar_path(entry))?;
        atomic_replace(&chunk_tmp, &self.chunk_path(entry))?;
        sync_dir(&self.root.join(SIDECARS_DIR))?;
        sync_dir(&self.root.join(CHUNKS_DIR))?;
        Ok(())
    }

    /// Read one entry's bytes and sidecar.
    ///
    /// Verifies on the way out, and a failure *drops* the entry rather than
    /// reporting an integrity error: the authoritative state is untouched by a
    /// bad cache line, and the caller's correct response is to fetch again.
    pub fn read(&self, entry: &[u8; 32]) -> Result<(Vec<u8>, Vec<u8>), CacheError> {
        let bytes = match std::fs::read(self.chunk_path(entry)) {
            Ok(b) => b,
            Err(_) => return Err(CacheError::NotResident),
        };
        let sidecar = match std::fs::read(self.sidecar_path(entry)) {
            Ok(b) => b,
            Err(_) => {
                self.drop_entry(entry);
                return Err(CacheError::NotResident);
            }
        };
        if crate::object_content_hash(&bytes) != *entry {
            self.drop_entry(entry);
            return Err(CacheError::Corrupt);
        }
        Ok((bytes, sidecar))
    }

    /// Whether an entry is advertisable: bytes and validated sidecar both here.
    pub fn is_resident(&self, entry: &[u8; 32]) -> bool {
        self.chunk_path(entry).exists() && self.sidecar_path(entry).exists()
    }

    /// A bounded range of one entry's bytes, without materialising the rest.
    pub fn read_range(
        &self,
        entry: &[u8; 32],
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, CacheError> {
        use std::io::{Read, Seek, SeekFrom};
        if len > crate::index::MAX_NODE_BYTES {
            return Err(CacheError::Bounds);
        }
        let mut file =
            std::fs::File::open(self.chunk_path(entry)).map_err(|_| CacheError::NotResident)?;
        if !self.sidecar_path(entry).exists() {
            return Err(CacheError::NotResident);
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| CacheError::Durability(e.to_string()))?;
        let mut out = vec![0u8; len];
        let read = file
            .read(&mut out)
            .map_err(|e| CacheError::Durability(e.to_string()))?;
        out.truncate(read);
        Ok(out)
    }

    fn drop_entry(&self, entry: &[u8; 32]) {
        let _ = std::fs::remove_file(self.chunk_path(entry));
        let _ = std::fs::remove_file(self.sidecar_path(entry));
    }

    /// Take a lease. Idempotent: the same operation taking the same lease twice
    /// holds it once.
    pub fn lease(&self, lease: &Lease) -> Result<(), CacheError> {
        let dir = self.root.join(TAGS_DIR);
        write_sync(&dir.join(lease.tag_name()), &[])?;
        sync_dir(&dir)?;
        Ok(())
    }

    /// Release a lease. Releasing one never collects anything on its own — a
    /// sweep decides — so a reader still holding the entry cannot be torn.
    pub fn release(&self, lease: &Lease) -> Result<(), CacheError> {
        match std::fs::remove_file(self.root.join(TAGS_DIR).join(lease.tag_name())) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("release lease", e).into()),
        }
        let _ = sync_dir(&self.root.join(TAGS_DIR));
        Ok(())
    }

    /// Release every lease an operation holds — what a cancelled or crashed
    /// transfer needs, and why lease names carry their operation.
    pub fn release_operation(&self, operation: &[u8; 16]) -> Result<u64, CacheError> {
        let mut released = 0;
        if let Ok(entries) = std::fs::read_dir(self.root.join(TAGS_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if Lease::parse(&name).is_some_and(|l| &l.operation == operation) {
                    let _ = std::fs::remove_file(entry.path());
                    released += 1;
                }
            }
        }
        let _ = sync_dir(&self.root.join(TAGS_DIR));
        Ok(released)
    }

    pub fn pin(&self, entry: &[u8; 32]) -> Result<(), CacheError> {
        let dir = self.root.join(TAGS_DIR);
        write_sync(&dir.join(pin_name(entry)), &[])?;
        sync_dir(&dir)?;
        Ok(())
    }

    pub fn unpin(&self, entry: &[u8; 32]) -> Result<(), CacheError> {
        match std::fs::remove_file(self.root.join(TAGS_DIR).join(pin_name(entry))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("unpin", e).into()),
        }
        let _ = sync_dir(&self.root.join(TAGS_DIR));
        Ok(())
    }

    /// Every entry currently held by a pin or a live lease.
    fn held(&self) -> BTreeSet<[u8; 32]> {
        let mut out = BTreeSet::new();
        if let Ok(entries) = std::fs::read_dir(self.root.join(TAGS_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(rest) = name.strip_prefix("pin.") {
                    if let Some(hash) = unhex(rest) {
                        out.insert(hash);
                    }
                } else if let Some(lease) = Lease::parse(&name) {
                    out.insert(lease.entry);
                }
            }
        }
        out
    }

    /// Open a staging slot for a partial transfer. Staged bytes live under an
    /// opaque id and are never advertised, so a half-arrived chunk cannot be
    /// mistaken for a resident one.
    pub fn stage(&self, operation: &[u8; 16], part: u32) -> PathBuf {
        self.root.join(STAGING_DIR).join(format!(
            "{}.{part}",
            data_encoding::HEXLOWER.encode(operation)
        ))
    }

    /// Append to a staging slot, returning its new length. The offset must be
    /// exactly the current length: a resumed transfer proves where it is rather
    /// than being trusted about it.
    pub fn append_staged(
        &self,
        operation: &[u8; 16],
        part: u32,
        offset: u64,
        bytes: &[u8],
    ) -> Result<u64, CacheError> {
        use std::io::Write;
        let path = self.stage(operation, part);
        let current = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if offset != current {
            return Err(CacheError::Corrupt);
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| CacheError::Durability(format!("stage: {e}")))?;
        file.write_all(bytes)
            .map_err(|e| CacheError::Durability(format!("stage write: {e}")))?;
        file.sync_all()
            .map_err(|e| CacheError::Durability(format!("stage fsync: {e}")))?;
        Ok(current + bytes.len() as u64)
    }

    pub fn read_staged(&self, operation: &[u8; 16], part: u32) -> Result<Vec<u8>, CacheError> {
        std::fs::read(self.stage(operation, part)).map_err(|_| CacheError::NotResident)
    }

    /// Discard everything an operation staged. A cancelled ingest or transfer
    /// leaves nothing durable behind it.
    pub fn discard_staged(&self, operation: &[u8; 16]) -> Result<u64, CacheError> {
        let prefix = data_encoding::HEXLOWER.encode(operation);
        let mut removed = 0;
        if let Ok(entries) = std::fs::read_dir(self.root.join(STAGING_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Drop one entry now, if nothing holds it.
    ///
    /// Distinct from [`Self::sweep`] on purpose. A sweep runs because space is
    /// tight and picks its own victims; this runs because a caller asked for
    /// these bytes to go, and a caller that asked should not have to wait for
    /// quota pressure that may never come. Both refuse to touch a held entry —
    /// "I want this gone" does not outrank "someone is reading it".
    pub fn evict(&self, entry: &[u8; 32]) -> Result<bool, CacheError> {
        if self.held().contains(entry) {
            return Ok(false);
        }
        self.drop_entry(entry);
        Ok(true)
    }

    /// Every resident entry's address. O(entries) and meant for a sweep or a
    /// scan, not a hot path.
    pub fn entries(&self) -> Vec<[u8; 32]> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(self.root.join(CHUNKS_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(hash) = unhex(&name) {
                    out.push(hash);
                }
            }
        }
        out.sort();
        out
    }

    /// Total resident bytes.
    pub fn resident_bytes(&self) -> u64 {
        let mut total = 0;
        for dir in [CHUNKS_DIR, SIDECARS_DIR] {
            if let Ok(entries) = std::fs::read_dir(self.root.join(dir)) {
                for entry in entries.flatten() {
                    if let Ok(meta) = entry.metadata() {
                        total += meta.len();
                    }
                }
            }
        }
        total
    }

    /// Drop half-installed pairs left by an interrupted run.
    fn reclaim_incomplete(&self) -> Result<(), CacheError> {
        let mut removed = Vec::new();
        for dir in [CHUNKS_DIR, SIDECARS_DIR] {
            if let Ok(entries) = std::fs::read_dir(self.root.join(dir)) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".tmp") {
                        let _ = std::fs::remove_file(entry.path());
                        continue;
                    }
                    if let Some(hash) = unhex(&name) {
                        if !self.chunk_path(&hash).exists() || !self.sidecar_path(&hash).exists() {
                            removed.push(hash);
                        }
                    } else {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
        for hash in removed {
            self.drop_entry(&hash);
        }
        Ok(())
    }

    /// Evict untagged, unleased entries until the cache is inside its quota.
    ///
    /// Quota-driven rather than time-driven: an entry is not stale because it is
    /// old, it is evictable because something else needs the room. Eviction
    /// order is largest-first, which reclaims the most room for the fewest
    /// refetches.
    pub fn sweep(&self) -> Result<SweepReport, CacheError> {
        let mut report = SweepReport::default();
        self.reclaim_incomplete()?;

        let held = self.held();
        let mut candidates: Vec<([u8; 32], u64)> = Vec::new();
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.root.join(CHUNKS_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(hash) = unhex(&name) else { continue };
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                total += len;
                if !held.contains(&hash) {
                    candidates.push((hash, len));
                }
            }
        }
        if total <= self.quota_bytes {
            return Ok(report);
        }

        candidates.sort_by_key(|(_, len)| std::cmp::Reverse(*len));
        for (hash, len) in candidates {
            if total <= self.quota_bytes {
                break;
            }
            self.drop_entry(&hash);
            total = total.saturating_sub(len);
            report.entries_removed += 1;
            report.bytes_reclaimed += len;
        }
        Ok(report)
    }

    /// Discard staging older than the caller's cutoff set — the caller decides
    /// which operations are dead, because only it knows.
    pub fn sweep_staging(&self, live: &BTreeSet<[u8; 16]>) -> Result<SweepReport, CacheError> {
        let mut report = SweepReport::default();
        let live_prefixes: BTreeMap<String, ()> = live
            .iter()
            .map(|op| (data_encoding::HEXLOWER.encode(op), ()))
            .collect();
        if let Ok(entries) = std::fs::read_dir(self.root.join(STAGING_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some((prefix, _)) = name.split_once('.') else {
                    let _ = std::fs::remove_file(entry.path());
                    report.staging_removed += 1;
                    continue;
                };
                if !live_prefixes.contains_key(prefix) {
                    report.bytes_reclaimed += entry.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = std::fs::remove_file(entry.path());
                    report.staging_removed += 1;
                }
            }
        }
        Ok(report)
    }

    /// The cache root, for callers that must place sibling state.
    pub fn root(&self) -> &Path {
        &self.root
    }
}
