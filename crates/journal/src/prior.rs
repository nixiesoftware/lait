//! Read-only access to the immediately preceding journal representation.
//!
//! This module is deliberately quarantined from [`crate::Store`]. Normal open
//! paths understand exactly one representation. A generation build may read
//! prior committed facts here, construct a fresh current store elsewhere, and
//! atomically activate that new generation only after semantic verification.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Defect, Failure, Object, Operation};

const FORMAT_VERSION: u8 = 1;
const MANIFEST_FILE: &str = "current-manifest";
const OBJECTS_DIR: &str = "objects";
const ACTIVE_JOURNAL: &str = "journal/active";
const MAX_MANIFEST: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    version: u8,
    sequence: u64,
    objects: Vec<Object>,
    meta: Vec<u8>,
}

/// A validated, committed store in the prior representation.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    manifest: Manifest,
}

impl Store {
    /// Open without recovery or mutation. A prior active journal is refused:
    /// only the code that authored that journal can interpret its crash phase.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Failure> {
        let root = root.into();
        if root.join(ACTIVE_JOURNAL).exists() {
            return Err(Failure::Integrity(Defect::CorruptJournal));
        }
        let bytes = std::fs::read(root.join(MANIFEST_FILE))
            .map_err(|error| crate::io_err(Operation::Read, error))?;
        if bytes.len() > MAX_MANIFEST {
            return Err(Failure::Integrity(Defect::CorruptManifest));
        }
        let manifest: Manifest = postcard::from_bytes(&bytes)
            .map_err(|_| Failure::Integrity(Defect::CorruptManifest))?;
        if manifest.version != FORMAT_VERSION
            || postcard::to_stdvec(&manifest)
                .map_err(|_| Failure::Integrity(Defect::CorruptManifest))?
                != bytes
        {
            return Err(Failure::Integrity(Defect::UnsupportedFormat));
        }

        let mut unique = BTreeSet::new();
        for object in &manifest.objects {
            if !unique.insert(object.hash) {
                return Err(Failure::Integrity(Defect::CorruptManifest));
            }
            validate_object(&root, object)?;
        }
        Ok(Self { root, manifest })
    }

    pub fn sequence(&self) -> u64 {
        self.manifest.sequence
    }

    pub fn meta(&self) -> &[u8] {
        &self.manifest.meta
    }

    pub fn objects(&self) -> &[Object] {
        &self.manifest.objects
    }

    pub fn read_object(&self, object: &Object) -> Result<Vec<u8>, Failure> {
        if !self.manifest.objects.contains(object) {
            return Err(Failure::Integrity(Defect::MissingObject));
        }
        read_object(&self.root, object)
    }
}

fn validate_object(root: &Path, object: &Object) -> Result<(), Failure> {
    read_object(root, object).map(|_| ())
}

fn read_object(root: &Path, object: &Object) -> Result<Vec<u8>, Failure> {
    let bytes = std::fs::read(root.join(OBJECTS_DIR).join(crate::hex(&object.hash)))
        .map_err(|_| Failure::Integrity(Defect::MissingObject))?;
    let len = u64::try_from(bytes.len()).map_err(|_| Failure::Integrity(Defect::CorruptObject))?;
    if len != object.len || crate::object_hash(&bytes) != object.hash {
        return Err(Failure::Integrity(Defect::CorruptObject));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> (PathBuf, Vec<u8>, Object) {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("lait-prior-journal-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(OBJECTS_DIR)).unwrap();
        std::fs::create_dir_all(root.join("journal")).unwrap();
        let bytes = b"signed fact".to_vec();
        let object = Object {
            hash: crate::object_hash(&bytes),
            len: u64::try_from(bytes.len()).unwrap(),
        };
        std::fs::write(
            root.join(OBJECTS_DIR).join(crate::hex(&object.hash)),
            &bytes,
        )
        .unwrap();
        let manifest = Manifest {
            version: FORMAT_VERSION,
            sequence: 9,
            objects: vec![object],
            meta: b"semantic index".to_vec(),
        };
        std::fs::write(
            root.join(MANIFEST_FILE),
            postcard::to_stdvec(&manifest).unwrap(),
        )
        .unwrap();
        (root, bytes, object)
    }

    #[test]
    fn reads_only_a_complete_valid_prior_commit() {
        let (root, bytes, object) = fixture();
        let store = Store::open(&root).unwrap();
        assert_eq!(store.sequence(), 9);
        assert_eq!(store.meta(), b"semantic index");
        assert_eq!(store.read_object(&object).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_missing_or_corrupt_material_and_an_active_journal() {
        let (root, _, object) = fixture();
        std::fs::write(
            root.join(OBJECTS_DIR).join(crate::hex(&object.hash)),
            b"wrong",
        )
        .unwrap();
        assert_eq!(
            Store::open(&root).unwrap_err(),
            Failure::Integrity(Defect::CorruptObject)
        );

        let (root, _, _) = fixture();
        std::fs::write(root.join(ACTIVE_JOURNAL), b"unresolved").unwrap();
        assert_eq!(
            Store::open(&root).unwrap_err(),
            Failure::Integrity(Defect::CorruptJournal)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
