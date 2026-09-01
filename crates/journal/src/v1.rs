//! The retired file-per-object layout, kept as a migration source.
//!
//! Until the pack log, a store root held `objects/` (one content-addressed
//! file per object), `current-manifest` (swapped by atomic rename), a 3-phase
//! intent journal under `journal/`, and a `counter` whose value survived even
//! crashed commits so a sequence could never be reissued. [`crate::Store`]
//! now serves the pack; this module exists so every store born under the old
//! layout is carried forward through one verified, crash-idempotent
//! migration — and so an *old* binary meeting a migrated root refuses it
//! legibly instead of forking it.
//!
//! The rules, each argued in the migration review:
//!
//! - **The source stays authoritative until a sealed pack exists.** Opening
//!   here runs the full V1 recovery — the intent journal is drained, the
//!   manifest and both requirement indexes validate, eager objects re-hash —
//!   and a store that fails validation fail-stops unmigrated: garbage is
//!   never carried forward.
//! - **The switch is the pack's seal plus one directory fsync.** The pack
//!   medium deliberately never syncs directories, so the *existence* of the
//!   slot file is not durable until the migration driver syncs the store
//!   root itself. Retirement may begin only after that.
//! - **Retirement is crash-idempotent and resumed at every open** until the
//!   source is fully moved into `retired-v1/` and the tombstone stands where
//!   `current-manifest` stood. The pack's first seal records the source's
//!   manifest hash and counter; a lingering source whose manifest no longer
//!   matches is a divergence and fail-stops — it is never silently retired.
//! - **The tombstone must fail an old binary safely.** It decodes as the
//!   *prior* generation (so old `Store::open` reports an unsupported format,
//!   not corruption) while naming a caller-meta object that cannot exist (so
//!   an old rebuild attempt fails loudly instead of succeeding empty).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    child, decode_manifest, decode_requirement, deferred_root, eager_root, hex, index, io_err,
    manifest_hash, object_content_hash, unhex, Defect, Failure, IoKind, Object, Operation,
    RequirementClass, COUNTER_FILE, JOURNAL_DIR, JOURNAL_FILE, MANIFEST_FILE, OBJECTS_DIR,
};

pub(crate) const RETIRED_DIR: &str = "retired-v1";

/// The journal phases. Each replaced `journal/active` atomically.
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
fn atomic_replace(tmp: &Path, dst: &Path) -> Result<(), Failure> {
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

/// Directory durability after a rename/create. On unix a real fsync; on
/// Windows a directory handle needs `FILE_FLAG_BACKUP_SEMANTICS`, and if none
/// opens at all, NTFS metadata journaling is the documented contract.
#[cfg(not(windows))]
pub(crate) fn sync_dir(dir: &Path) -> Result<(), Failure> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_err(Operation::Sync, e))
}

#[cfg(windows)]
pub(crate) fn sync_dir(dir: &Path) -> Result<(), Failure> {
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
        Err(_) => Ok(()),
        Ok(d) => d.sync_all().map_err(|e| io_err(Operation::Sync, e)),
    }
}

/// Read one object file under the caller's admitted bound, checking file
/// length before allocation and trailing EOF plus content address after.
pub(crate) fn read_file_bounded(
    path: &Path,
    hash: &[u8; 32],
    expected_len: u64,
    max_len: u64,
) -> Result<Vec<u8>, Failure> {
    if expected_len > max_len {
        return Err(Failure::Integrity(Defect::CorruptObject));
    }
    let mut file = File::open(path).map_err(|error| {
        tracing::warn!(%error, object = %hex(hash), "journal object is absent");
        if error.kind() == std::io::ErrorKind::NotFound {
            Failure::Integrity(Defect::MissingObject)
        } else {
            io_err(Operation::Read, error)
        }
    })?;
    let stored_len = file
        .metadata()
        .map_err(|error| io_err(Operation::Read, error))?
        .len();
    if stored_len != expected_len {
        return Err(Failure::Integrity(Defect::CorruptObject));
    }
    let capacity =
        usize::try_from(expected_len).map_err(|_| Failure::Integrity(Defect::CorruptObject))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| Failure::Operation {
            operation: Operation::Read,
            kind: IoKind::Other,
        })?;
    bytes.resize(capacity, 0);
    file.read_exact(&mut bytes)
        .map_err(|_| Failure::Integrity(Defect::CorruptObject))?;
    let mut trailing = [0u8; 1];
    match file.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err(Failure::Integrity(Defect::CorruptObject)),
        Err(error) => return Err(io_err(Operation::Read, error)),
    }
    if object_content_hash(&bytes) != *hash {
        return Err(Failure::Integrity(Defect::CorruptObject));
    }
    Ok(bytes)
}

/// Reads index nodes out of the object directory.
struct FileNodes<'a> {
    root: &'a Path,
}

impl index::NodeSource for FileNodes<'_> {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.root.join(OBJECTS_DIR).join(hex(hash))).ok()?;
        (object_content_hash(&bytes) == *hash).then_some(bytes)
    }
}

/// Whether a root still carries old-layout material that needs acting on.
/// The tombstone alone is settled history, not presence.
pub(crate) fn present(root: &Path) -> bool {
    if root.join(JOURNAL_DIR).join(JOURNAL_FILE).exists()
        || root.join(OBJECTS_DIR).is_dir()
        || root.join(COUNTER_FILE).exists()
    {
        return true;
    }
    match std::fs::read(root.join(MANIFEST_FILE)) {
        Ok(bytes) => !is_tombstone(&bytes),
        Err(_) => false,
    }
}

/// Whether the root's manifest position holds the tombstone.
pub(crate) fn tombstoned(root: &Path) -> bool {
    std::fs::read(root.join(MANIFEST_FILE)).is_ok_and(|bytes| is_tombstone(&bytes))
}

/// A fully recovered, fully validated old-layout store, ready to stream.
pub(crate) struct Source {
    root: PathBuf,
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) counter: u64,
    pub(crate) objects: Vec<[u8; 32]>,
    eager: Option<index::ChildRef>,
    deferred: Option<index::ChildRef>,
}

impl Source {
    /// Open the old layout for migration: drain the intent journal exactly as
    /// the old recovery did, then validate everything the old open validated.
    /// A source that fails validation fail-stops here, unmigrated.
    pub(crate) fn open(root: &Path) -> Result<Self, Failure> {
        drain_journal(root)?;
        let bytes = match std::fs::read(root.join(MANIFEST_FILE)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A layout with objects but no manifest never committed:
                // there is nothing to carry, and the empty pack is faithful.
                return Ok(Self {
                    root: root.to_path_buf(),
                    manifest_bytes: Vec::new(),
                    counter: read_counter(root, false)?,
                    objects: Vec::new(),
                    eager: None,
                    deferred: None,
                });
            }
            Err(e) => return Err(io_err(Operation::Read, e)),
        };
        if is_tombstone(&bytes) {
            // Migration already happened; only retirement remains. The
            // caller resolves that against the pack's provenance.
            return Err(Failure::Integrity(Defect::UnsupportedFormat));
        }
        let manifest = decode_manifest(&bytes)?;
        let source = FileNodes { root };
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
        let mut bad = false;
        index::stream(&source, eager_root(&manifest), &mut |entry| {
            if decode_requirement(&entry.value, RequirementClass::Eager).is_none() {
                bad = true;
            }
        })
        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        if bad {
            return Err(Failure::Integrity(Defect::CorruptIndex));
        }
        let counter = read_counter(root, true)?;
        if counter < manifest.sequence {
            return Err(Failure::Integrity(Defect::MissingCounter));
        }
        let mut objects = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root.join(OBJECTS_DIR)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(hash) = unhex(&name) {
                    objects.push(hash);
                }
            }
        }
        objects.sort_unstable();
        Ok(Self {
            root: root.to_path_buf(),
            manifest_bytes: bytes,
            counter,
            objects,
            eager: eager_root(&manifest),
            deferred: deferred_root(&manifest),
        })
    }

    /// Read one object, verified against its content address. Migration
    /// re-hashes every byte it carries — a copy is a claim.
    pub(crate) fn read_object(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Failure> {
        let bytes = std::fs::read(self.root.join(OBJECTS_DIR).join(hex(hash)))
            .map_err(|e| io_err(Operation::Read, e))?;
        if object_content_hash(&bytes) != *hash {
            return Err(Failure::Integrity(Defect::CorruptObject));
        }
        Ok(bytes)
    }

    /// Whether the exposed state requires this object. What validation has
    /// already proven whole fail-stops on rot; an orphan outside the promise
    /// is skippable history.
    pub(crate) fn is_required(&self, hash: &[u8; 32]) -> Result<bool, Failure> {
        crate::requirement_length(
            &FileNodes { root: &self.root },
            self.eager,
            self.deferred,
            hash,
        )
        .map(|len| len.is_some())
    }

    /// Total object bytes, for the space preflight.
    pub(crate) fn total_bytes(&self) -> u64 {
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(self.root.join(OBJECTS_DIR)) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
        total
    }

    pub(crate) fn manifest_hash(&self) -> [u8; 32] {
        manifest_hash(&self.manifest_bytes)
    }
}

/// The old recovery's journal arm, verbatim in effect: every phase resolves
/// to "the on-disk manifest decides", and the journal file is removed.
fn drain_journal(root: &Path) -> Result<(), Failure> {
    let journal_path = root.join(JOURNAL_DIR).join(JOURNAL_FILE);
    match std::fs::read(&journal_path) {
        Ok(bytes) => {
            let _: JournalRecord = postcard::from_bytes(&bytes)
                .map_err(|_| Failure::Integrity(Defect::CorruptJournal))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(Operation::Read, e)),
    }
    match std::fs::remove_file(&journal_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(Operation::Remove, e)),
    }
    let _ = sync_dir(&root.join(JOURNAL_DIR));
    Ok(())
}

fn read_counter(root: &Path, manifest_present: bool) -> Result<u64, Failure> {
    match File::open(root.join(COUNTER_FILE)) {
        Ok(mut f) => {
            let mut buf = [0u8; 8];
            f.read_exact(&mut buf)
                .map_err(|_| Failure::Integrity(Defect::MissingCounter))?;
            Ok(u64::from_le_bytes(buf))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if manifest_present {
                // A store with a manifest and no counter could reuse
                // sequences: fail closed, exactly as the old open did.
                return Err(Failure::Integrity(Defect::MissingCounter));
            }
            Ok(0)
        }
        Err(e) => Err(io_err(Operation::Read, e)),
    }
}

/// The bytes left where `current-manifest` stood. They decode as the *prior*
/// store generation, so an old binary's open answers "unsupported format" —
/// the message that says rebuild-or-restore, not the one that says damage —
/// and the caller-meta object they name cannot exist, so an old rebuild
/// attempt fails on a missing object instead of succeeding empty.
pub(crate) fn tombstone_bytes() -> Vec<u8> {
    let marker = crate::PriorIndexedManifest {
        format_version: crate::PRIOR_STORE_FORMAT_VERSION,
        sequence: u64::MAX,
        required_object_index_root: None,
        caller_meta: Some(Object {
            hash: object_content_hash(b"lait: this store moved to the pack format"),
            len: u64::MAX,
        }),
        caller_index_roots: Vec::new(),
    };
    postcard::to_stdvec(&marker).unwrap_or_default()
}

pub(crate) fn is_tombstone(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes == tombstone_bytes().as_slice()
}

/// Move the old layout aside, crash-idempotently: every step is "move it if
/// it is still here", and the tombstone lands last. Resumed at every open
/// until nothing old remains in place.
pub(crate) fn retire(root: &Path) -> Result<(), Failure> {
    let retired = root.join(RETIRED_DIR);
    std::fs::create_dir_all(&retired).map_err(|e| io_err(Operation::Open, e))?;
    for piece in [OBJECTS_DIR, JOURNAL_DIR, COUNTER_FILE] {
        let from = root.join(piece);
        if !from.exists() {
            continue;
        }
        let to = retired.join(piece);
        if to.exists() {
            // A half-moved piece from a prior attempt: the copy that made it
            // into retirement wins; what lingers outside is a duplicate.
            let _ = std::fs::remove_dir_all(&from);
            let _ = std::fs::remove_file(&from);
            continue;
        }
        std::fs::rename(&from, &to).map_err(|e| io_err(Operation::Rename, e))?;
    }
    let manifest = root.join(MANIFEST_FILE);
    let needs_tombstone = match std::fs::read(&manifest) {
        Ok(bytes) => {
            if !is_tombstone(&bytes) {
                let kept = retired.join(MANIFEST_FILE);
                if !kept.exists() {
                    std::fs::copy(&manifest, &kept).map_err(|e| io_err(Operation::Write, e))?;
                }
                true
            } else {
                false
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(io_err(Operation::Read, e)),
    };
    if needs_tombstone {
        let tmp = root.join(format!("{MANIFEST_FILE}.tmp"));
        write_sync(&tmp, &tombstone_bytes())?;
        atomic_replace(&tmp, &manifest)?;
    }
    sync_dir(root)?;
    Ok(())
}

/// How many bytes the filesystem under `root` still offers.
#[cfg(unix)]
#[allow(
    clippy::unnecessary_fallible_conversions,
    clippy::useless_conversion,
    reason = "statvfs field widths differ across unix platforms"
)]
pub(crate) fn available_bytes(root: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(root.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is a zeroed
    // out-parameter of the exact type statvfs fills.
    if unsafe { libc::statvfs(path.as_ptr(), &raw mut stat) } != 0 {
        return None;
    }
    u64::try_from(stat.f_bavail)
        .ok()?
        .checked_mul(u64::try_from(stat.f_frsize).ok()?)
}

#[cfg(windows)]
pub(crate) fn available_bytes(root: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut free: u64 = 0;
    // SAFETY: `wide` is NUL-terminated and `free` is a valid out-parameter.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn available_bytes(_root: &Path) -> Option<u64> {
    // No old layout can exist on a target that never ran the old format;
    // migration is unreachable here, and the preflight with it.
    None
}
