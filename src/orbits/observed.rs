//! What this device last saw a Space named, written beside the Space's store.
//!
//! The registry keeps coordinates and refuses contents (`crate::orbits`), and
//! the picker names a Space only from a live Station (`crate::serve::orbits`).
//! Between them is a Space this device holds but has not opened since its
//! daemon started: the row has an id and nothing else, and the person is left
//! to click it to learn which of their Spaces it is.
//!
//! A closed replica receives no renames. So "the name this device last saw"
//! is exactly as current as the bytes on disk, and no read of a closed store
//! could know more. What made a remembered name wrong before was that it was
//! written once, at founding, and served *as* the name. This record is
//! neither: the Station holding the store lock writes it when it starts
//! serving, on every commit that touches the Space's own Catalog, and when it
//! stops; and the row carries it as `seen`, beside `name`, with the time it
//! was seen -- an observation, never a claim.
//!
//! It lives in the Orbit's store directory rather than the registry, so a
//! store that is deleted takes its observation with it, and so the one writer
//! is the one process that holds the store. Losing it costs one click; a
//! reader that cannot decode it reports nothing rather than something.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The record's format. A reader that meets another number reports nothing.
pub const FORMAT: u8 = 1;
/// The file, beside `marker` and `epoch` in the Orbit's store directory.
pub const FILE: &str = "observed-name.json";
/// Our own bound. The Catalog does not bound a Space's name; a reading past
/// this is refused rather than written, and a file past it is not read.
pub const MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    pub format: u8,
    /// The Catalog name as the Station read it.
    pub name: String,
    /// Unix seconds when it was read.
    pub observed_at: u64,
    /// The daemon epoch and Observation sequence the reading followed, when
    /// one did; zero for a reading taken as a Station started or stopped.
    #[serde(default)]
    pub epoch: u64,
    #[serde(default)]
    pub sequence: u64,
}

pub fn path(store_dir: &Path) -> PathBuf {
    store_dir.join(FILE)
}

static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// Write the record: temp file, fsync, atomic replace, directory sync -- the
/// same discipline as a World update record (`crate::update::consent`).
///
/// Into a store directory that exists, never one this creates: a Station
/// whose store was deleted under it stops, and a record written on the way
/// out would leave a directory that reads as a store with nothing in it.
pub fn write(store_dir: &Path, observed: &Observed) -> Result<()> {
    let bytes = serde_json::to_vec(observed).context("encode observed name")?;
    anyhow::ensure!(
        u64::try_from(bytes.len()).is_ok_and(|len| len <= MAX_BYTES),
        "observed name exceeds its {MAX_BYTES} byte bound"
    );
    anyhow::ensure!(
        store_dir.is_dir(),
        "no store at {} to record beside",
        store_dir.display()
    );
    let path = path(store_dir);
    let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("tmp.{}.{}", std::process::id(), nonce));
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
    }
    crate::update::consent::commit_replace(&temporary, &path, store_dir)
        .with_context(|| format!("commit {}", path.display()))
}

/// The record, or nothing.
///
/// Nothing for a missing file, an oversized one, one that does not decode,
/// and one of another format alike: every one of those is "not observed", and
/// the row says so by carrying no `seen`. None of them is an error a lister
/// should refuse a page over.
pub fn read(store_dir: &Path) -> Option<Observed> {
    let path = path(store_dir);
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let observed: Observed = serde_json::from_slice(&bytes).ok()?;
    (observed.format == FORMAT).then_some(observed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lait-observed-{tag}-{}-{}",
            std::process::id(),
            TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_written_observation_reads_back() {
        let dir = dir("roundtrip");
        let observed = Observed {
            format: FORMAT,
            name: "Kas".into(),
            observed_at: 1_700_000_000,
            epoch: 3,
            sequence: 41,
        };
        write(&dir, &observed).unwrap();
        assert_eq!(read(&dir), Some(observed.clone()));
        let later = Observed {
            name: "Kas, renamed".into(),
            ..observed
        };
        write(&dir, &later).unwrap();
        assert_eq!(read(&dir), Some(later));
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == FILE),
            "no temporary survives a write"
        );
    }

    #[test]
    fn anything_that_is_not_a_reading_is_no_reading() {
        let dir = dir("absent");
        assert_eq!(read(&dir), None, "missing");
        std::fs::write(path(&dir), b"{not json").unwrap();
        assert_eq!(read(&dir), None, "corrupt");
        std::fs::write(
            path(&dir),
            serde_json::to_vec(&Observed {
                format: FORMAT + 1,
                name: "Kas".into(),
                observed_at: 1,
                epoch: 0,
                sequence: 0,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(read(&dir), None, "another format");
        std::fs::write(
            path(&dir),
            vec![b' '; usize::try_from(MAX_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert_eq!(read(&dir), None, "oversized");
    }

    #[test]
    fn a_record_is_written_beside_a_store_and_never_in_its_place() {
        let dir = dir("gone").join("ws_gone");
        assert!(!dir.exists());
        let observed = Observed {
            format: FORMAT,
            name: "Gone".into(),
            observed_at: 1,
            epoch: 0,
            sequence: 0,
        };
        assert!(write(&dir, &observed).is_err(), "no store, no record");
        assert!(!dir.exists(), "a record must not conjure a store directory");
    }
}
