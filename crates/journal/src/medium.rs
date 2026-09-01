//! The physical seam under the pack log: a handful of slots that only ever
//! append, read at an offset, flush, and truncate.
//!
//! This is deliberately smaller than a filesystem. The five verbs here are the
//! ones every target provides with honest semantics — a native file, an OPFS
//! `FileSystemSyncAccessHandle` in a worker, a buffer in memory. What is
//! *missing* is the point: no rename, no directory sync, no per-object file
//! creation. Those are the three operations the measured platforms punish —
//! rename has no portable atomicity story in a browser, directory sync already
//! needed a Windows apology arm in this crate, and object-per-file creation is
//! the exact anti-pattern SQLite's OPFS pool VFS exists to avoid. The pack
//! log's crash safety comes from ordered appends and checksummed seals
//! instead, so the seam never has to promise what a platform cannot keep.
//!
//! A **slot** is one named append-only byte sequence. A store uses O(1) of
//! them — the live pack generation, and its successor during compaction — so
//! opening a slot is a cold-path operation. On a target where obtaining a
//! handle is asynchronous (OPFS), a medium is constructed over a pool of
//! pre-opened handles and `open_slot` only assigns one.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One named append-only byte sequence.
///
/// Offsets are stable forever: an append returns the offset the bytes landed
/// at, and `read_at` answers for any offset below `len`. `truncate` discards
/// the tail — recovery's one mutation — and never grows.
pub trait Slot: Send {
    /// Total bytes currently in the slot.
    fn len(&self) -> Result<u64, std::io::Error>;
    /// Whether the slot holds no bytes.
    fn is_empty(&self) -> Result<bool, std::io::Error> {
        Ok(self.len()? == 0)
    }
    /// Read exactly `buf.len()` bytes starting at `offset`. Reading past the
    /// end is an error, never a short read.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error>;
    /// Append `bytes`, returning the offset they begin at. Durability is
    /// claimed by [`Slot::flush`], not here.
    fn append(&mut self, bytes: &[u8]) -> Result<u64, std::io::Error>;
    /// Make everything appended so far durable. The pack log calls this once
    /// per commit, which is the entire fsync budget of the design.
    fn flush(&mut self) -> Result<(), std::io::Error>;
    /// Discard every byte at and after `new_len`. Growing is refused.
    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error>;
}

/// A namespace of slots. `open_slot` creates on first open; a slot, once
/// opened, is exclusively held by its caller.
pub trait Medium: Send {
    fn open_slot(&self, name: &str) -> Result<Box<dyn Slot>, std::io::Error>;
    /// Remove a slot's bytes and its name. Absence is success: removal is
    /// cleanup, and cleanup that already happened is not a failure.
    fn remove_slot(&self, name: &str) -> Result<(), std::io::Error>;
    /// Every slot name currently present, in unspecified order.
    fn slot_names(&self) -> Result<Vec<String>, std::io::Error>;
}

/// The native medium: one directory, one file per slot.
#[derive(Debug)]
pub struct DirMedium {
    root: PathBuf,
}

impl DirMedium {
    /// Open a directory as a medium, creating it if absent.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }
}

/// Slot names come from the pack log's fixed vocabulary, but refuse path
/// separators anyway: a slot is a name, never a path.
fn checked_name(name: &str) -> Result<&str, std::io::Error> {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c == '/' || c == '\\' || c == '.' || c == '\0')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "slot names are single path components",
        ));
    }
    Ok(name)
}

struct FileSlot {
    file: std::fs::File,
}

impl Slot for FileSlot {
    fn len(&self) -> Result<u64, std::io::Error> {
        Ok(self.file.metadata()?.len())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        // Positional reads exist on unix (`FileExt::read_exact_at`) but not
        // portably; a seeking read on a `&File` clone of the descriptor is the
        // portable spelling. The pack log is single-owner, so the shared
        // cursor races with nobody.
        let mut handle = &self.file;
        handle.seek(SeekFrom::Start(offset))?;
        handle.read_exact(buf)
    }

    fn append(&mut self, bytes: &[u8]) -> Result<u64, std::io::Error> {
        let offset = self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(bytes)?;
        Ok(offset)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.file.sync_all()
    }

    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error> {
        if new_len > self.file.metadata()?.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "truncate never grows a slot",
            ));
        }
        self.file.set_len(new_len)
    }
}

impl Medium for DirMedium {
    fn open_slot(&self, name: &str) -> Result<Box<dyn Slot>, std::io::Error> {
        let path = self.root.join(checked_name(name)?);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(Box::new(FileSlot { file }))
    }

    fn remove_slot(&self, name: &str) -> Result<(), std::io::Error> {
        match std::fs::remove_file(self.root.join(checked_name(name)?)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn slot_names(&self) -> Result<Vec<String>, std::io::Error> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }
}

/// The in-memory medium: the wasm-runnable backend today, the test double
/// always. Shared by cloning; a reopened store on the same value sees what a
/// "process" before it flushed — and, deliberately, also what it merely
/// appended, because memory has no crash. Torn-tail cases are constructed by
/// truncating explicitly.
#[derive(Debug, Clone, Default)]
pub struct MemMedium {
    slots: Arc<Mutex<BTreeMap<String, SlotBytes>>>,
}

type SlotBytes = Arc<Mutex<Vec<u8>>>;

impl MemMedium {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn poisoned() -> std::io::Error {
    std::io::Error::other("medium lock poisoned")
}

struct MemSlot {
    bytes: SlotBytes,
}

impl Slot for MemSlot {
    fn len(&self) -> Result<u64, std::io::Error> {
        let len = self.bytes.lock().map_err(|_| poisoned())?.len();
        Ok(u64::try_from(len).unwrap_or(u64::MAX))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        let out_of_range = || std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        let start = usize::try_from(offset).map_err(|_| out_of_range())?;
        let end = start.checked_add(buf.len()).ok_or_else(out_of_range)?;
        let bytes = self.bytes.lock().map_err(|_| poisoned())?;
        buf.copy_from_slice(bytes.get(start..end).ok_or_else(out_of_range)?);
        Ok(())
    }

    fn append(&mut self, appended: &[u8]) -> Result<u64, std::io::Error> {
        let mut bytes = self.bytes.lock().map_err(|_| poisoned())?;
        let offset = u64::try_from(bytes.len())
            .map_err(|_| std::io::Error::other("slot beyond addressable length"))?;
        bytes.extend_from_slice(appended);
        drop(bytes);
        Ok(offset)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        Ok(())
    }

    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error> {
        let new_len = usize::try_from(new_len)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let mut bytes = self.bytes.lock().map_err(|_| poisoned())?;
        if new_len > bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "truncate never grows a slot",
            ));
        }
        bytes.truncate(new_len);
        Ok(())
    }
}

impl Medium for MemMedium {
    fn open_slot(&self, name: &str) -> Result<Box<dyn Slot>, std::io::Error> {
        let mut slots = self.slots.lock().map_err(|_| poisoned())?;
        let bytes = slots
            .entry(checked_name(name)?.to_owned())
            .or_default()
            .clone();
        Ok(Box::new(MemSlot { bytes }))
    }

    fn remove_slot(&self, name: &str) -> Result<(), std::io::Error> {
        let mut slots = self.slots.lock().map_err(|_| poisoned())?;
        slots.remove(checked_name(name)?);
        Ok(())
    }

    fn slot_names(&self) -> Result<Vec<String>, std::io::Error> {
        let slots = self.slots.lock().map_err(|_| poisoned())?;
        Ok(slots.keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media() -> Vec<(&'static str, Box<dyn Medium>)> {
        let dir = std::env::temp_dir().join(format!(
            "lait-medium-{}-{}",
            std::process::id(),
            crate::hex(&crate::object_content_hash(
                format!("{:?}", std::time::Instant::now()).as_bytes()
            ))
        ));
        vec![
            ("dir", Box::new(DirMedium::open(dir).unwrap())),
            ("mem", Box::new(MemMedium::new())),
        ]
    }

    #[test]
    fn every_medium_agrees_on_the_five_verbs() {
        for (name, medium) in media() {
            let mut slot = medium.open_slot("pack-a").unwrap();
            assert!(slot.is_empty().unwrap(), "{name}: fresh slot is empty");
            let first = slot.append(b"alpha").unwrap();
            let second = slot.append(b"beta").unwrap();
            assert_eq!((first, second), (0, 5), "{name}: appends report offsets");
            slot.flush().unwrap();

            let mut buf = [0u8; 4];
            slot.read_at(5, &mut buf).unwrap();
            assert_eq!(&buf, b"beta", "{name}: read_at answers at offset");
            assert!(
                slot.read_at(6, &mut buf).is_err(),
                "{name}: a read past the end is an error, never short"
            );

            slot.truncate(5).unwrap();
            assert_eq!(slot.len().unwrap(), 5, "{name}: truncate discards the tail");
            assert!(
                slot.truncate(9).is_err(),
                "{name}: truncate never grows a slot"
            );

            // Reopening the same name resumes the same bytes.
            drop(slot);
            let slot = medium.open_slot("pack-a").unwrap();
            assert_eq!(slot.len().unwrap(), 5, "{name}: a slot survives reopen");

            assert_eq!(
                medium.slot_names().unwrap(),
                vec!["pack-a".to_owned()],
                "{name}: names list what exists"
            );
            medium.remove_slot("pack-a").unwrap();
            medium
                .remove_slot("pack-a")
                .expect("removing an absent slot is cleanup, not failure");
            assert!(medium.slot_names().unwrap().is_empty(), "{name}: removed");
        }
    }

    #[test]
    fn a_slot_name_is_never_a_path() {
        for (name, medium) in media() {
            for bad in ["", "a/b", "a\\b", "..", "a\0b"] {
                assert!(
                    medium.open_slot(bad).is_err(),
                    "{name}: {bad:?} must be refused"
                );
            }
        }
    }
}
