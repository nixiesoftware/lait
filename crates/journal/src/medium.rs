//! The physical seam under the pack log: named slots that append, read at an
//! offset, flush, and truncate — and nothing else.
//!
//! This is deliberately smaller than a filesystem. These are the verbs every
//! target provides with honest semantics — a native file, an OPFS
//! `FileSystemSyncAccessHandle` in a worker, a buffer in memory. What is
//! *missing* is the point: no rename, no directory sync, no per-object file
//! creation. Those are the operations the measured platforms punish, so the
//! pack log's crash safety comes from ordered appends and checksummed seals
//! instead, and this seam never has to promise what a platform cannot keep.
//!
//! A slot opens as **two halves**: an exclusive [`SlotWriter`] and a shared
//! [`ReadAt`]. Readers on other threads read while the writer appends, so
//! **no operation ever reads a cursor** — every read and write names its
//! absolute offset, and the writer's tracked length is the sole append
//! authority. (Unix gets pread/pwrite; Windows `seek_read`/`seek_write` move
//! the shared cursor, which is harmless exactly because nothing reads it;
//! OPFS always passes `{at}`.) The layer above keeps reader ranges and
//! writer-mutated ranges disjoint — see the invariants on
//! [`crate::PackStore`] — which is why positional non-atomicity never
//! matters.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The shared read half of a slot. Reading past the end is an error, never a
/// short read. Offsets are stable for the life of the slot's bytes.
pub trait ReadAt: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error>;
}

/// The exclusive write half of a slot.
///
/// `len` is tracked, not asked: it is established when the slot opens and
/// only this writer moves it. `truncate` discards the tail — recovery's one
/// mutation — and never grows. Durability is claimed by `flush` alone.
pub trait SlotWriter: Send {
    fn len(&self) -> u64;
    /// Append `bytes` at the tracked length, returning the offset they begin
    /// at.
    fn append(&mut self, bytes: &[u8]) -> Result<u64, std::io::Error>;
    fn flush(&mut self) -> Result<(), std::io::Error>;
    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error>;
}

/// A namespace of slots. `open_slot` creates on first open; a slot's write
/// half, once opened, is exclusively held by its caller.
pub trait Medium: Send + Sync {
    fn open_slot(
        &self,
        name: &str,
    ) -> Result<(Box<dyn SlotWriter>, Arc<dyn ReadAt>), std::io::Error>;
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

struct FileReadAt {
    file: Arc<File>,
}

impl ReadAt for FileReadAt {
    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        std::os::unix::fs::FileExt::read_exact_at(self.file.as_ref(), buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        // `seek_read` moves the shared cursor; that is benign because no
        // operation on either half ever reads it.
        let mut at = offset;
        let mut rest = buf;
        while !rest.is_empty() {
            let n = std::os::windows::fs::FileExt::seek_read(self.file.as_ref(), rest, at)?;
            if n == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
            }
            at = at
                .checked_add(u64::try_from(n).unwrap_or(u64::MAX))
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
            rest = rest.get_mut(n..).unwrap_or(&mut []);
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> Result<(), std::io::Error> {
        // A target with no positional file API has no filesystem here at all;
        // the browser medium is OPFS, not this one.
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

struct FileWriter {
    file: Arc<File>,
    len: u64,
}

impl FileWriter {
    #[cfg(unix)]
    fn write_all_at(&self, bytes: &[u8], offset: u64) -> Result<(), std::io::Error> {
        std::os::unix::fs::FileExt::write_all_at(self.file.as_ref(), bytes, offset)
    }

    #[cfg(windows)]
    fn write_all_at(&self, bytes: &[u8], offset: u64) -> Result<(), std::io::Error> {
        let mut at = offset;
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = std::os::windows::fs::FileExt::seek_write(self.file.as_ref(), rest, at)?;
            if n == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
            }
            at = at
                .checked_add(u64::try_from(n).unwrap_or(u64::MAX))
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::WriteZero))?;
            rest = rest.get(n..).unwrap_or(&[]);
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn write_all_at(&self, _bytes: &[u8], _offset: u64) -> Result<(), std::io::Error> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

impl SlotWriter for FileWriter {
    fn len(&self) -> u64 {
        self.len
    }

    fn append(&mut self, bytes: &[u8]) -> Result<u64, std::io::Error> {
        let offset = self.len;
        self.write_all_at(bytes, offset)?;
        self.len = offset
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("slot beyond addressable length"))?;
        Ok(offset)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.file.sync_all()
    }

    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error> {
        if new_len > self.len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "truncate never grows a slot",
            ));
        }
        self.file.set_len(new_len)?;
        self.len = new_len;
        Ok(())
    }
}

impl Medium for DirMedium {
    fn open_slot(
        &self,
        name: &str,
    ) -> Result<(Box<dyn SlotWriter>, Arc<dyn ReadAt>), std::io::Error> {
        let path = self.root.join(checked_name(name)?);
        // Never `append(true)`: on Linux, pwrite on an O_APPEND descriptor
        // ignores its offset (documented), which would break the positional
        // discipline silently.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let len = file.metadata()?.len();
        let file = Arc::new(file);
        Ok((
            Box::new(FileWriter {
                file: file.clone(),
                len,
            }),
            Arc::new(FileReadAt { file }),
        ))
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

struct MemReadAt {
    bytes: SlotBytes,
}

impl ReadAt for MemReadAt {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        let out_of_range = || std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        let start = usize::try_from(offset).map_err(|_| out_of_range())?;
        let end = start.checked_add(buf.len()).ok_or_else(out_of_range)?;
        let bytes = self.bytes.lock().map_err(|_| poisoned())?;
        buf.copy_from_slice(bytes.get(start..end).ok_or_else(out_of_range)?);
        Ok(())
    }
}

struct MemWriter {
    bytes: SlotBytes,
    len: u64,
}

impl SlotWriter for MemWriter {
    fn len(&self) -> u64 {
        self.len
    }

    fn append(&mut self, appended: &[u8]) -> Result<u64, std::io::Error> {
        let mut bytes = self.bytes.lock().map_err(|_| poisoned())?;
        let offset = self.len;
        // The map may hold more than the tracked length (a test constructed
        // a torn tail); the append lands at the tracked length regardless.
        let at = usize::try_from(offset).map_err(|_| poisoned())?;
        bytes.truncate(at);
        bytes.extend_from_slice(appended);
        drop(bytes);
        self.len = offset
            .checked_add(u64::try_from(appended.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("slot beyond addressable length"))?;
        Ok(offset)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        Ok(())
    }

    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error> {
        if new_len > self.len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "truncate never grows a slot",
            ));
        }
        let at = usize::try_from(new_len)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let mut bytes = self.bytes.lock().map_err(|_| poisoned())?;
        bytes.truncate(at);
        drop(bytes);
        self.len = new_len;
        Ok(())
    }
}

impl Medium for MemMedium {
    fn open_slot(
        &self,
        name: &str,
    ) -> Result<(Box<dyn SlotWriter>, Arc<dyn ReadAt>), std::io::Error> {
        let mut slots = self.slots.lock().map_err(|_| poisoned())?;
        let bytes = slots
            .entry(checked_name(name)?.to_owned())
            .or_default()
            .clone();
        drop(slots);
        let len = {
            let held = bytes.lock().map_err(|_| poisoned())?;
            u64::try_from(held.len()).unwrap_or(u64::MAX)
        };
        Ok((
            Box::new(MemWriter {
                bytes: bytes.clone(),
                len,
            }),
            Arc::new(MemReadAt { bytes }),
        ))
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
    fn every_medium_agrees_on_the_verbs() {
        for (name, medium) in media() {
            let (mut writer, read) = medium.open_slot("pack-0").unwrap();
            assert_eq!(writer.len(), 0, "{name}: fresh slot is empty");
            let first = writer.append(b"alpha").unwrap();
            let second = writer.append(b"beta").unwrap();
            assert_eq!((first, second), (0, 5), "{name}: appends report offsets");
            writer.flush().unwrap();

            let mut buf = [0u8; 4];
            read.read_at(5, &mut buf).unwrap();
            assert_eq!(&buf, b"beta", "{name}: read_at answers at offset");
            assert!(
                read.read_at(6, &mut buf).is_err(),
                "{name}: a read past the end is an error, never short"
            );

            writer.truncate(5).unwrap();
            assert_eq!(writer.len(), 5, "{name}: truncate discards the tail");
            assert!(
                writer.truncate(9).is_err(),
                "{name}: truncate never grows a slot"
            );
            // The read half sees the writer's mutations at once.
            assert!(read.read_at(5, &mut buf[..1]).is_err(), "{name}");

            // Reopening the same name resumes the same bytes.
            drop(writer);
            drop(read);
            let (writer, _) = medium.open_slot("pack-0").unwrap();
            assert_eq!(writer.len(), 5, "{name}: a slot survives reopen");
            drop(writer);

            assert_eq!(
                medium.slot_names().unwrap(),
                vec!["pack-0".to_owned()],
                "{name}: names list what exists"
            );
            medium.remove_slot("pack-0").unwrap();
            medium
                .remove_slot("pack-0")
                .expect("removing an absent slot is cleanup, not failure");
            assert!(medium.slot_names().unwrap().is_empty(), "{name}: removed");
        }
    }

    #[test]
    fn readers_and_writer_share_one_slot_without_a_cursor() {
        for (name, medium) in media() {
            let (mut writer, read) = medium.open_slot("pack-0").unwrap();
            writer.append(b"0123456789").unwrap();
            let read_two = read.clone();
            let mut a = [0u8; 2];
            let mut b = [0u8; 2];
            // Interleaved positional reads and appends: nothing here depends
            // on a cursor, so every answer is by offset alone.
            read.read_at(0, &mut a).unwrap();
            writer.append(b"AB").unwrap();
            read_two.read_at(8, &mut b).unwrap();
            writer.append(b"CD").unwrap();
            read.read_at(10, &mut a).unwrap();
            assert_eq!((&b, &a), (b"89", b"AB"), "{name}");
            assert_eq!(writer.len(), 14, "{name}");
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
