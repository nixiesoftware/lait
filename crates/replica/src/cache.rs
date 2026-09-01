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
//! **One file per entry.** The bytes and the sidecar that proves them live in
//! a single file, so a rename publishes both or neither. They used to be two
//! files published by two renames, which meant a real window in which the
//! sidecar existed and the chunk did not — indistinguishable from the wreckage
//! of an interrupted run, and reclaimed as such by a concurrent sweep. An entry
//! that half-exists is the one state this cache must not be able to reach.
//!
//! **The caller names the slot; the entry carries its own address.** An entry
//! used to be filed under the hash of its own bytes, which sounds right and
//! costs a great deal: a caller holding a descriptor cannot compute the hash of
//! a chunk it has not fetched, so answering "which chunks do I have" meant
//! reading and hashing the entire cache. A slot is whatever 32 bytes the caller
//! derives — for content, from the root and the index — so residency becomes a
//! question about *this* content rather than a scan of everything. Integrity is
//! not given up for it: the file header carries the bytes' content address, and
//! every read checks it.
//!
//! Like the rest of this crate it is semantics-free. It stores blobs under
//! 32-byte slots, each with a sidecar blob attached, and named tags that keep
//! them alive. It does not know what content is, what a proof proves, or why
//! anything is pinned.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(windows))]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use journal::{Failure as JournalFailure, IoKind, Operation};

const ENTRIES_DIR: &str = "chunks";
const TAGS_DIR: &str = "tags";
const STAGING_DIR: &str = "staging";
const MAX_ENTRY_BYTES: usize = 1024 * 1024;

fn io_err(operation: Operation, error: std::io::Error) -> JournalFailure {
    tracing::warn!(%error, ?operation, "residency operation failed");
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => IoKind::NotFound,
        std::io::ErrorKind::PermissionDenied => IoKind::PermissionDenied,
        std::io::ErrorKind::Interrupted => IoKind::Interrupted,
        std::io::ErrorKind::InvalidData => IoKind::InvalidData,
        _ => IoKind::Other,
    };
    JournalFailure::Operation { operation, kind }
}

fn write_sync(path: &Path, bytes: &[u8]) -> Result<(), JournalFailure> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| io_err(Operation::Open, error))?;
    file.write_all(bytes)
        .map_err(|error| io_err(Operation::Write, error))?;
    file.sync_all()
        .map_err(|error| io_err(Operation::Sync, error))
}

fn atomic_replace(source: &Path, destination: &Path) -> Result<(), JournalFailure> {
    let mut last = None;
    for attempt in 0..5 {
        match std::fs::rename(source, destination) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
                }
            }
        }
    }
    match last {
        Some(error) => Err(io_err(Operation::Rename, error)),
        None => Ok(()),
    }
}

// Open-and-sync is portable std; on a target with no filesystem at all
// (wasm32-unknown-unknown) the open fails, which is the honest answer —
// see `journal::sync_dir`, whose windows arm this one also mirrors.
#[cfg(not(windows))]
fn sync_dir(path: &Path) -> Result<(), JournalFailure> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_err(Operation::Sync, error))
}

#[cfg(windows)]
fn sync_dir(path: &Path) -> Result<(), JournalFailure> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .or_else(|_| {
            OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
        });
    match handle {
        Err(_) => Ok(()),
        Ok(directory) => directory
            .sync_all()
            .map_err(|error| io_err(Operation::Sync, error)),
    }
}

/// The fixed header on an entry file: the sidecar's length, then the content
/// address of the bytes that follow it.
const ENTRY_HEADER_LEN: usize = 4 + 32;

/// Why a cache operation failed.
///
/// Deliberately distinct from [`JournalFailure`]: a missing resident entry is
/// **not** an integrity failure. Collapsing the two is what would let a
/// refetchable chunk take a store down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The entry is not here. Expected, and usually means "go fetch it".
    NotResident,
    /// The bytes are here but do not match the address they are filed under, or
    /// the entry file is malformed. The entry is dropped and becomes
    /// refetchable.
    Corrupt,
    /// A durable write or read failed. Distinct from `Corrupt` on purpose: a
    /// transient I/O failure is not evidence that the bytes are wrong, and
    /// treating it as such deletes good data on a bad day.
    Durability(JournalFailure),
    /// A bound was exceeded before anything was allocated.
    Bounds,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotResident => write!(f, "not resident"),
            Self::Corrupt => write!(f, "corrupt cache entry"),
            Self::Durability(failure) => write!(f, "durability: {failure}"),
            Self::Bounds => write!(f, "bounds"),
        }
    }
}
impl std::error::Error for Failure {}

impl From<JournalFailure> for Failure {
    fn from(e: JournalFailure) -> Self {
        Self::Durability(e)
    }
}

/// What kind of holder took a lease.
///
/// The two live in one tag directory but must never release each other. An
/// operation id is a local, ephemeral handle; a content nonce is a public field
/// of a replicated descriptor, so anything that could be persuaded to treat a
/// nonce as an operation id would be a way to drop another holder's bytes. They
/// are both `[u8; 16]`, so only the kind keeps them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LeaseKind {
    /// One in-flight operation: a read, an ingest, a transfer.
    Operation,
    /// One committed content, keyed by its nonce. Released by reachability.
    Content,
}

impl LeaseKind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Operation => "op",
            Self::Content => "content",
        }
    }
}

/// A hold on one entry, keyed by whoever took it.
///
/// Keyed per *holder*, not per entry: two concurrent fetches of the same
/// content are two leases over one chunk set, and the bytes survive until the
/// last of them releases. A lease keyed by content id alone would let the first
/// transfer to finish sweep the second's staged bytes.
///
/// The name is derived rather than stored, so an interrupted operation's holds
/// are recoverable after restart without a side file to keep consistent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lease {
    pub kind: LeaseKind,
    pub holder: [u8; 16],
    pub entry: [u8; 32],
}

impl Lease {
    /// A hold for the duration of one operation.
    #[must_use]
    pub const fn operation(operation: [u8; 16], entry: [u8; 32]) -> Self {
        Self {
            kind: LeaseKind::Operation,
            holder: operation,
            entry,
        }
    }

    /// A hold that lasts as long as the content is reachable.
    #[must_use]
    pub const fn content(content_nonce: [u8; 16], entry: [u8; 32]) -> Self {
        Self {
            kind: LeaseKind::Content,
            holder: content_nonce,
            entry,
        }
    }

    pub(crate) fn tag_name(&self) -> String {
        format!(
            "{}.{}.{}",
            self.kind.prefix(),
            data_encoding::HEXLOWER.encode(&self.holder),
            data_encoding::HEXLOWER.encode(&self.entry)
        )
    }

    fn parse(name: &str) -> Option<Self> {
        let (kind, rest) = name.split_once('.')?;
        let kind = match kind {
            "op" => LeaseKind::Operation,
            "content" => LeaseKind::Content,
            _ => return None,
        };
        let (holder, entry) = rest.split_once('.')?;
        let holder = data_encoding::HEXLOWER.decode(holder.as_bytes()).ok()?;
        let entry = data_encoding::HEXLOWER.decode(entry.as_bytes()).ok()?;
        Some(Self {
            kind,
            holder: <[u8; 16]>::try_from(holder.as_slice()).ok()?,
            entry: <[u8; 32]>::try_from(entry.as_slice()).ok()?,
        })
    }
}

/// A durable pin: a caller's statement that an entry should survive quota
/// pressure. Unlike a lease it has no holder and no expiry.
fn pin_name(entry: &[u8; 32]) -> String {
    format!("pin.{}", data_encoding::HEXLOWER.encode(entry))
}

/// What a sweep did, so a caller can report rather than guess.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub entries_removed: u64,
    pub bytes_reclaimed: u64,
    pub staging_removed: u64,
    /// How far over quota the cache still is once every eligible victim is
    /// gone.
    ///
    /// A sweep that cannot reach the quota is the normal case, not an error:
    /// committed content holds its own chunks resident, and a hold outranks
    /// operator policy. But a sweep that reports success while sitting 32× over
    /// quota tells the operator nothing, so the shortfall is reported rather
    /// than swallowed.
    pub over_quota_bytes: u64,
}

/// The resident cache.
pub struct Residency {
    root: PathBuf,
    /// Maximum resident bytes before a sweep starts evicting. Operator policy.
    quota_bytes: u64,
    /// How many residency questions this cache has been asked.
    ///
    /// Counted because "a range read costs the range, not the content" is a
    /// complexity claim, and a complexity claim nothing can observe is a
    /// comment. Each probe is a filesystem stat, so the counter is free beside
    /// what it counts.
    probes: std::sync::atomic::AtomicU64,
}

fn hex(hash: &[u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(hash)
}

fn unhex(name: &str) -> Option<[u8; 32]> {
    let raw = data_encoding::HEXLOWER.decode(name.as_bytes()).ok()?;
    <[u8; 32]>::try_from(raw.as_slice()).ok()
}

/// Split a stored entry file into its declared byte address, its sidecar, and
/// its bytes.
fn split_entry(raw: &[u8]) -> Option<([u8; 32], &[u8], &[u8])> {
    if raw.len() < ENTRY_HEADER_LEN {
        return None;
    }
    let sidecar_len = usize::try_from(u32::from_le_bytes(raw.get(..4)?.try_into().ok()?)).ok()?;
    let address: [u8; 32] = raw.get(4..ENTRY_HEADER_LEN)?.try_into().ok()?;
    let body = raw.get(ENTRY_HEADER_LEN..)?;
    if sidecar_len > body.len() {
        return None;
    }
    let (sidecar, bytes) = body.split_at(sidecar_len);
    Some((address, sidecar, bytes))
}

impl Residency {
    /// Open (creating) a cache under `root`, reclaiming anything a previous run
    /// left half-written.
    pub fn open(root: impl Into<PathBuf>, quota_bytes: u64) -> Result<Self, Failure> {
        let root = root.into();
        for dir in [ENTRIES_DIR, TAGS_DIR, STAGING_DIR] {
            std::fs::create_dir_all(root.join(dir))
                .map_err(|error| io_err(Operation::Open, error))?;
        }
        let cache = Self {
            root,
            quota_bytes,
            probes: std::sync::atomic::AtomicU64::new(0),
        };
        cache.reclaim_incomplete()?;
        Ok(cache)
    }

    pub const fn quota_bytes(&self) -> u64 {
        self.quota_bytes
    }

    fn entry_path(&self, entry: &[u8; 32]) -> PathBuf {
        self.root.join(ENTRIES_DIR).join(hex(entry))
    }

    /// Install one entry into `slot`: its bytes and the sidecar that proves
    /// them.
    ///
    /// The bytes' content address goes in the header, the whole thing is
    /// written and fsynced as one temporary, and one rename publishes it. An
    /// interruption leaves a temporary, which the next open reclaims — there is
    /// no state in which the entry exists without its sidecar.
    ///
    /// The slot is the caller's to choose and this call does not interpret it.
    /// What it guarantees is that whatever comes back out of a slot is
    /// byte-identical to what went in; whether those are the *right* bytes for
    /// that slot is the caller's question, and the sidecar is how it answers.
    pub fn install(&self, slot: &[u8; 32], bytes: &[u8], sidecar: &[u8]) -> Result<(), Failure> {
        let sidecar_len = u32::try_from(sidecar.len()).map_err(|_| Failure::Bounds)?;
        let capacity = ENTRY_HEADER_LEN
            .checked_add(sidecar.len())
            .and_then(|len| len.checked_add(bytes.len()))
            .ok_or(Failure::Bounds)?;
        let mut raw = Vec::with_capacity(capacity);
        raw.extend_from_slice(&sidecar_len.to_le_bytes());
        raw.extend_from_slice(&journal::object_content_hash(bytes));
        raw.extend_from_slice(sidecar);
        raw.extend_from_slice(bytes);

        let path = self.entry_path(slot);
        let tmp = path.with_extension("tmp");
        write_sync(&tmp, &raw)?;
        atomic_replace(&tmp, &path)?;
        sync_dir(&self.root.join(ENTRIES_DIR))?;
        Ok(())
    }

    /// Read one slot's bytes and sidecar.
    ///
    /// Verifies against the address stored beside them. Bytes that fail it
    /// *drop* the entry rather than reporting an integrity error: the
    /// authoritative state is untouched by a bad cache line, and the caller's
    /// correct response is to fetch again. A transient read failure is a
    /// different answer — it is not evidence the entry is wrong, and deleting on
    /// it would turn a busy filesystem into data loss.
    pub fn read(&self, slot: &[u8; 32]) -> Result<(Vec<u8>, Vec<u8>), Failure> {
        let raw = match std::fs::read(self.entry_path(slot)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Failure::NotResident),
            Err(error) => return Err(io_err(Operation::Read, error).into()),
        };
        let Some((address, sidecar, bytes)) = split_entry(&raw) else {
            self.drop_entry(slot);
            return Err(Failure::Corrupt);
        };
        if journal::object_content_hash(bytes) != address {
            self.drop_entry(slot);
            return Err(Failure::Corrupt);
        }
        Ok((bytes.to_vec(), sidecar.to_vec()))
    }

    /// Whether an entry is advertisable.
    ///
    /// Presence is the whole answer, because [`Self::install`] is the only door
    /// and it verifies before it publishes — an entry that exists was validated
    /// when it landed. This is a cheap check by design; [`Self::read`] is the
    /// one that re-verifies, and it is what actually serves bytes.
    /// Whether these bytes are here.
    ///
    /// `Result` because `exists()` answers `false` for a directory it could not
    /// read, and a Station that cannot probe its own cache would report holding
    /// nothing — then refetch everything it already has. `held` refuses to
    /// answer "nothing" on error for the same reason; this used not to.
    pub fn is_resident(&self, entry: &[u8; 32]) -> Result<bool, Failure> {
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.entry_path(entry)
            .try_exists()
            .map_err(|error| io_err(Operation::Read, error).into())
    }

    /// How many times [`Self::is_resident`] has been asked, since this cache
    /// was opened.
    ///
    /// Monotonic and never reset: a caller measuring one operation reads it
    /// either side and subtracts, which is the only reading that means anything
    /// when several callers share a cache.
    pub fn residency_probes(&self) -> u64 {
        self.probes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A bounded range of one entry's bytes.
    ///
    /// Verifies the whole entry first. A chunk is a quarter of a megabyte, so
    /// the alternative — seeking into an unverified file — buys almost nothing
    /// and gives up the property that every byte this cache hands out has been
    /// checked against its address.
    pub fn read_range(
        &self,
        entry: &[u8; 32],
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, Failure> {
        if len > MAX_ENTRY_BYTES {
            return Err(Failure::Bounds);
        }
        let (bytes, _) = self.read(entry)?;
        let start = usize::try_from(offset).map_err(|_| Failure::Bounds)?;
        if start > bytes.len() {
            return Err(Failure::Bounds);
        }
        let end = start.saturating_add(len).min(bytes.len());
        bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(Failure::Bounds)
    }

    /// Remove an entry's file. Reports whether it is actually gone, because
    /// both callers act on the answer: a sweep counts bytes it believes it
    /// freed, and an eviction tells a caller its request was carried out.
    fn drop_entry(&self, entry: &[u8; 32]) -> bool {
        match std::fs::remove_file(self.entry_path(entry)) {
            Ok(()) => true,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }

    /// Take a lease. Idempotent: the same holder taking the same lease twice
    /// holds it once.
    pub(crate) fn lease(&self, lease: &Lease) -> Result<(), Failure> {
        let dir = self.root.join(TAGS_DIR);
        write_sync(&dir.join(lease.tag_name()), &[])?;
        sync_dir(&dir)?;
        Ok(())
    }

    /// Retain an entry for one in-flight operation.
    pub fn hold_operation(&self, operation: [u8; 16], entry: [u8; 32]) -> Result<(), Failure> {
        self.lease(&Lease::operation(operation, entry))
    }

    /// Retain an entry while its content descriptor remains reachable.
    pub fn hold_content(&self, content: [u8; 16], entry: [u8; 32]) -> Result<(), Failure> {
        self.lease(&Lease::content(content, entry))
    }

    /// Release a lease. Releasing one never collects anything on its own — a
    /// sweep decides — so a reader still holding the entry cannot be torn.
    pub(crate) fn release(&self, lease: &Lease) -> Result<(), Failure> {
        match std::fs::remove_file(self.root.join(TAGS_DIR).join(lease.tag_name())) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(Operation::Remove, e).into()),
        }
        let _ = sync_dir(&self.root.join(TAGS_DIR));
        Ok(())
    }

    /// Release every lease an operation holds — what a cancelled or crashed
    /// transfer needs, and why lease names carry their holder.
    ///
    /// Operation holds only. A content hold that happened to share the same
    /// sixteen bytes is a different kind of promise and survives.
    pub fn release_operation(&self, operation: &[u8; 16]) -> Result<u64, Failure> {
        self.release_holder(LeaseKind::Operation, operation)
    }

    /// Release one holder's hold on one entry.
    ///
    /// The symmetric partner of `hold_*`, which a sliding window needs: a
    /// reader letting go of what it has passed must not let go of what it has
    /// not reached. Pins cannot express it — they are per entry, so one reader
    /// unpinning would unpin another's chunk.
    pub fn release_operation_entry(
        &self,
        operation: [u8; 16],
        entry: [u8; 32],
    ) -> Result<(), Failure> {
        let path = self
            .root
            .join(TAGS_DIR)
            .join(Lease::operation(operation, entry).tag_name());
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(Operation::Remove, e).into()),
        }
        let _ = sync_dir(&self.root.join(TAGS_DIR));
        Ok(())
    }

    /// Release every hold one content's nonce took, which is what forgetting a
    /// content means.
    pub fn release_content(&self, content_nonce: &[u8; 16]) -> Result<u64, Failure> {
        self.release_holder(LeaseKind::Content, content_nonce)
    }

    fn release_holder(&self, kind: LeaseKind, holder: &[u8; 16]) -> Result<u64, Failure> {
        let mut released = 0u64;
        let dir = self.root.join(TAGS_DIR);
        let entries = std::fs::read_dir(&dir).map_err(|error| io_err(Operation::Read, error))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if Lease::parse(&name).is_some_and(|l| l.kind == kind && &l.holder == holder) {
                let _ = std::fs::remove_file(entry.path());
                released = released.saturating_add(1);
            }
        }
        let _ = sync_dir(&dir);
        Ok(released)
    }

    pub fn pin(&self, entry: &[u8; 32]) -> Result<(), Failure> {
        let dir = self.root.join(TAGS_DIR);
        write_sync(&dir.join(pin_name(entry)), &[])?;
        sync_dir(&dir)?;
        Ok(())
    }

    pub fn unpin(&self, entry: &[u8; 32]) -> Result<(), Failure> {
        match std::fs::remove_file(self.root.join(TAGS_DIR).join(pin_name(entry))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(Operation::Remove, e).into()),
        }
        let _ = sync_dir(&self.root.join(TAGS_DIR));
        Ok(())
    }

    /// Whether this entry carries a durable pin.
    pub fn is_pinned(&self, entry: &[u8; 32]) -> bool {
        self.root.join(TAGS_DIR).join(pin_name(entry)).exists()
    }

    /// Whether anything currently holds this entry.
    pub fn is_held(&self, entry: &[u8; 32]) -> Result<bool, Failure> {
        Ok(self.held()?.contains(entry))
    }

    /// Every entry currently held by a pin or a live lease.
    ///
    /// Fails rather than answering "nothing". This is the one read that decides
    /// whether deletion is allowed, so an unreadable tag directory has to stop
    /// the sweep — returning an empty set on error would make every pinned and
    /// leased entry collectable at exactly the moment the filesystem is unwell.
    fn held(&self) -> Result<BTreeSet<[u8; 32]>, Failure> {
        let mut out = BTreeSet::new();
        let entries = std::fs::read_dir(self.root.join(TAGS_DIR))
            .map_err(|error| io_err(Operation::Read, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io_err(Operation::Read, error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix("pin.") {
                if let Some(hash) = unhex(rest) {
                    out.insert(hash);
                }
            } else if let Some(lease) = Lease::parse(&name) {
                out.insert(lease.entry);
            }
        }
        Ok(out)
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
    ) -> Result<u64, Failure> {
        use std::io::Write;
        let path = self.stage(operation, part);
        let current = std::fs::metadata(&path).map_or(0, |m| m.len());
        if offset != current {
            return Err(Failure::Corrupt);
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| io_err(Operation::Open, error))?;
        file.write_all(bytes)
            .map_err(|error| io_err(Operation::Write, error))?;
        file.sync_all()
            .map_err(|error| io_err(Operation::Sync, error))?;
        let appended = u64::try_from(bytes.len()).map_err(|_| Failure::Bounds)?;
        current.checked_add(appended).ok_or(Failure::Bounds)
    }

    /// How much one staging slot already holds. What a resumed transfer asks
    /// before it decides where to continue from.
    pub fn staged_len(&self, operation: &[u8; 16], part: u32) -> u64 {
        std::fs::metadata(self.stage(operation, part)).map_or(0, |m| m.len())
    }

    /// Total bytes across every staging slot.
    ///
    /// Staged bytes are real disk that the quota does not see: an entry is not
    /// resident until it installs, so a fleet of half-finished transfers can
    /// fill a disk while the cache reports itself comfortably inside its
    /// ceiling. A caller that stages needs its own budget, and this is what it
    /// checks against.
    pub fn staged_bytes(&self) -> u64 {
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.root.join(STAGING_DIR)) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
        total
    }

    pub fn read_staged(&self, operation: &[u8; 16], part: u32) -> Result<Vec<u8>, Failure> {
        std::fs::read(self.stage(operation, part)).map_err(|_| Failure::NotResident)
    }

    /// Discard one staging slot.
    ///
    /// Distinct from [`Self::discard_staged`], which is prefix-matched over the
    /// whole operation. A transfer that installed its third chunk and then
    /// discarded *the operation* would delete the partials for every other
    /// chunk still in flight — so finishing one part has to say so.
    pub fn discard_staged_part(&self, operation: &[u8; 16], part: u32) -> Result<(), Failure> {
        match std::fs::remove_file(self.stage(operation, part)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_err(Operation::Remove, error).into()),
        }
    }

    /// Discard everything an operation staged. A cancelled ingest or transfer
    /// leaves nothing durable behind it.
    pub fn discard_staged(&self, operation: &[u8; 16]) -> Result<u64, Failure> {
        let prefix = data_encoding::HEXLOWER.encode(operation);
        let mut removed = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.root.join(STAGING_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                    removed = removed.saturating_add(1);
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
    pub fn evict(&self, entry: &[u8; 32]) -> Result<bool, Failure> {
        if self.held()?.contains(entry) {
            return Ok(false);
        }
        Ok(self.drop_entry(entry))
    }

    /// Every resident entry's address. O(entries) and meant for a sweep or a
    /// scan, not a hot path.
    pub fn entries(&self) -> Vec<[u8; 32]> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(self.root.join(ENTRIES_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(hash) = unhex(&name) {
                    out.push(hash);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Total resident bytes.
    pub fn resident_bytes(&self) -> u64 {
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.root.join(ENTRIES_DIR)) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
        total
    }

    /// Drop temporaries left by an interrupted run.
    ///
    /// There is nothing else to reclaim: an entry is one file published by one
    /// rename, so the only wreckage a crash can leave is a temporary that was
    /// never renamed.
    fn reclaim_incomplete(&self) -> Result<(), Failure> {
        if let Ok(entries) = std::fs::read_dir(self.root.join(ENTRIES_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".tmp") || unhex(&name).is_none() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    /// Evict untagged, unleased entries until the cache is inside its quota.
    ///
    /// Quota-driven rather than time-driven: an entry is not stale because it is
    /// old, it is evictable because something else needs the room. Eviction
    /// order is largest-first, which reclaims the most room for the fewest
    /// refetches.
    ///
    /// The quota is a target, not a guarantee, and the gap is reported rather
    /// than hidden: every chunk of a committed content is held by that
    /// content's own lease, so a cache full of committed content has no
    /// eligible victims at all. `over_quota_bytes` is what an operator needs to
    /// see to know the answer is "unpin or forget something", not "wait".
    pub fn sweep(&self) -> Result<SweepReport, Failure> {
        self.sweep_to(self.quota_bytes)
    }

    /// Reclaim until `bytes` of headroom exists beneath the quota.
    ///
    /// [`Self::sweep`] reclaims *to* the quota, which is the wrong question for
    /// anything about to add to it: a caller admitting a transfer is over only
    /// once the bytes land, so a sweep that stops at the line always answers
    /// "nothing to do" and the caller refuses itself. Demand-paged reads fill
    /// the cache with their own wake and need exactly this.
    pub fn reclaim_for(&self, bytes: u64) -> Result<SweepReport, Failure> {
        self.sweep_to(self.quota_bytes.saturating_sub(bytes))
    }

    fn sweep_to(&self, target: u64) -> Result<SweepReport, Failure> {
        let mut report = SweepReport::default();
        self.reclaim_incomplete()?;

        let held = self.held()?;
        let mut candidates: Vec<([u8; 32], u64)> = Vec::new();
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.root.join(ENTRIES_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(hash) = unhex(&name) else { continue };
                let len = entry.metadata().map_or(0, |m| m.len());
                total = total.saturating_add(len);
                if !held.contains(&hash) {
                    candidates.push((hash, len));
                }
            }
        }
        if total <= target {
            return Ok(report);
        }

        candidates.sort_by_key(|(_, len)| std::cmp::Reverse(*len));
        for (hash, len) in candidates {
            if total <= target {
                break;
            }
            // Only count what actually went. A removal that failed leaves the
            // bytes on disk, and a report that counted them would have the
            // caller believe the cache is inside a quota it is not.
            if !self.drop_entry(&hash) {
                continue;
            }
            total = total.saturating_sub(len);
            report.entries_removed = report.entries_removed.saturating_add(1);
            report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(len);
        }
        report.over_quota_bytes = total.saturating_sub(self.quota_bytes);
        Ok(report)
    }

    /// Release every operation lease no live operation holds.
    ///
    /// An operation lease outlives the process that took it — that is the point
    /// of deriving the tag name rather than storing a side table, and it is
    /// what lets an interrupted transfer be resumed rather than restarted. The
    /// cost is that a transfer killed by a crash holds its chunks resident
    /// forever unless someone says the operation is over, and only the caller
    /// knows which operations are still live.
    ///
    /// Content holds are untouched: they belong to committed content, not to an
    /// operation, and no restart makes them stale.
    pub fn sweep_leases(&self, live: &BTreeSet<[u8; 16]>) -> Result<u64, Failure> {
        let mut released = 0u64;
        let dir = self.root.join(TAGS_DIR);
        let entries = std::fs::read_dir(&dir).map_err(|error| io_err(Operation::Read, error))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(lease) = Lease::parse(&name) else {
                continue;
            };
            if lease.kind == LeaseKind::Operation && !live.contains(&lease.holder) {
                let _ = std::fs::remove_file(entry.path());
                released = released.saturating_add(1);
            }
        }
        let _ = sync_dir(&dir);
        Ok(released)
    }

    /// Discard staging older than the caller's cutoff set — the caller decides
    /// which operations are dead, because only it knows.
    pub fn sweep_staging(&self, live: &BTreeSet<[u8; 16]>) -> Result<SweepReport, Failure> {
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
                    report.staging_removed = report.staging_removed.saturating_add(1);
                    continue;
                };
                if !live_prefixes.contains_key(prefix) {
                    report.bytes_reclaimed = report
                        .bytes_reclaimed
                        .saturating_add(entry.metadata().map_or(0, |m| m.len()));
                    let _ = std::fs::remove_file(entry.path());
                    report.staging_removed = report.staging_removed.saturating_add(1);
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
