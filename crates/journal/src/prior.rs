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

const VECTOR_FORMAT_VERSION: u8 = 1;
const INDEXED_FORMAT_VERSION: u8 = 2;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IndexedManifest {
    format_version: u8,
    sequence: u64,
    required_object_index_root: Option<([u8; 32], u64)>,
    caller_meta: Option<Object>,
    caller_index_roots: Vec<([u8; 32], u64)>,
}

#[derive(Debug)]
enum SourceManifest {
    Vector(Manifest),
    Indexed(IndexedManifest),
}

/// One canonical entry streamed from a caller index committed by an indexed
/// prior store. The migration reader exposes values without exposing the
/// Journal's internal radix-node vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerIndexEntry {
    pub key: [u8; 32],
    pub value: Vec<u8>,
}

/// One bounded authenticated caller-index page. `next` is the last returned
/// index key and can be supplied as the exclusive cursor for the next page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerIndexPage {
    pub entries: Vec<CallerIndexEntry>,
    pub next: Option<[u8; 32]>,
}

/// A validated, committed store in the prior representation.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    manifest: SourceManifest,
    meta: Vec<u8>,
}

struct Nodes<'a>(&'a Path);

impl crate::index::NodeSource for Nodes<'_> {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.0.join(OBJECTS_DIR).join(crate::hex(hash))).ok()?;
        (crate::object_hash(&bytes) == *hash).then_some(bytes)
    }
}

fn child(root: ([u8; 32], u64)) -> crate::index::ChildRef {
    crate::index::ChildRef {
        hash: root.0,
        count: root.1,
    }
}

fn decode_len(value: &[u8]) -> Option<u64> {
    <[u8; 8]>::try_from(value).ok().map(u64::from_be_bytes)
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
        if let Ok(manifest) = postcard::from_bytes::<IndexedManifest>(&bytes) {
            if manifest.format_version == INDEXED_FORMAT_VERSION
                && postcard::to_stdvec(&manifest)
                    .map_err(|_| Failure::Integrity(Defect::CorruptManifest))?
                    == bytes
            {
                let nodes = Nodes(&root);
                let required = manifest.required_object_index_root.map(child);
                crate::index::validate_root(&nodes, required)
                    .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
                for caller in &manifest.caller_index_roots {
                    crate::index::validate_root(&nodes, Some(child(*caller)))
                        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
                }
                let meta = match manifest.caller_meta {
                    Some(reference) => read_object(&root, &reference)?,
                    None => Vec::new(),
                };
                return Ok(Self {
                    root,
                    manifest: SourceManifest::Indexed(manifest),
                    meta,
                });
            }
        }

        let manifest: Manifest = postcard::from_bytes(&bytes)
            .map_err(|_| Failure::Integrity(Defect::CorruptManifest))?;
        if manifest.version != VECTOR_FORMAT_VERSION
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
        let meta = manifest.meta.clone();
        Ok(Self {
            root,
            manifest: SourceManifest::Vector(manifest),
            meta,
        })
    }

    pub fn sequence(&self) -> u64 {
        match &self.manifest {
            SourceManifest::Vector(manifest) => manifest.sequence,
            SourceManifest::Indexed(manifest) => manifest.sequence,
        }
    }

    pub fn meta(&self) -> &[u8] {
        &self.meta
    }

    pub fn object_count(&self) -> u64 {
        match &self.manifest {
            SourceManifest::Vector(manifest) => {
                u64::try_from(manifest.objects.len()).unwrap_or(u64::MAX)
            }
            SourceManifest::Indexed(manifest) => manifest
                .required_object_index_root
                .map_or(0, |(_, count)| count),
        }
    }

    /// Caller-index roots authenticated by the prior v2 manifest.
    pub fn caller_index_roots(&self) -> Vec<([u8; 32], u64)> {
        match &self.manifest {
            SourceManifest::Vector(_) => Vec::new(),
            SourceManifest::Indexed(manifest) => manifest.caller_index_roots.clone(),
        }
    }

    /// Stream one exact prior caller index without rendering its full entry
    /// set. Only a root committed by the source manifest is accepted.
    pub fn for_each_caller_index_entry(
        &self,
        root: ([u8; 32], u64),
        mut visit: impl FnMut(CallerIndexEntry) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let SourceManifest::Indexed(manifest) = &self.manifest else {
            return Err(Failure::Integrity(Defect::CorruptIndex));
        };
        if !manifest.caller_index_roots.contains(&root) {
            return Err(Failure::Integrity(Defect::CorruptIndex));
        }
        let mut failure = None;
        crate::index::stream(&Nodes(&self.root), Some(child(root)), &mut |entry| {
            if failure.is_some() {
                return;
            }
            if let Err(error) = visit(CallerIndexEntry {
                key: entry.key,
                value: entry.value.clone(),
            }) {
                failure = Some(error);
            }
        })
        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }

    /// Seek a committed caller index without replaying its returned prefix.
    /// One extra entry is authenticated internally to determine continuation.
    pub fn caller_index_page(
        &self,
        root: ([u8; 32], u64),
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> Result<CallerIndexPage, Failure> {
        let SourceManifest::Indexed(manifest) = &self.manifest else {
            return Err(Failure::Integrity(Defect::CorruptIndex));
        };
        if !manifest.caller_index_roots.contains(&root) || !(1..=4096).contains(&limit) {
            return Err(Failure::Integrity(Defect::CorruptIndex));
        }
        let take = usize::from(limit);
        let mut entries = crate::index::page_after(
            &Nodes(&self.root),
            Some(child(root)),
            after.as_ref(),
            take.saturating_add(1),
        )
        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
        let has_more = entries.len() > take;
        entries.truncate(take);
        let next = has_more
            .then(|| entries.last().map(|entry| entry.key))
            .flatten();
        Ok(CallerIndexPage {
            entries: entries
                .into_iter()
                .map(|entry| CallerIndexEntry {
                    key: entry.key,
                    value: entry.value,
                })
                .collect(),
            next,
        })
    }

    /// Authenticate one exact caller-index path. This is used to cross-check
    /// paired catalogs (for example Body records against their advertised
    /// manifest entries) without streaming either catalog.
    pub fn caller_index_lookup(
        &self,
        root: ([u8; 32], u64),
        key: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, Failure> {
        let SourceManifest::Indexed(manifest) = &self.manifest else {
            return Err(Failure::Integrity(Defect::CorruptIndex));
        };
        if !manifest.caller_index_roots.contains(&root) {
            return Err(Failure::Integrity(Defect::CorruptIndex));
        }
        crate::index::lookup_validated(&Nodes(&self.root), Some(child(root)), key, &|value| {
            value.len() <= crate::index::MAX_VALUE_BYTES
        })
        .map_err(|_| Failure::Integrity(Defect::CorruptIndex))
    }

    /// Stream the prior required set without rendering an O(objects) vector.
    pub fn for_each_object(
        &self,
        mut visit: impl FnMut(Object) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        match &self.manifest {
            SourceManifest::Vector(manifest) => {
                for object in &manifest.objects {
                    visit(*object)?;
                }
            }
            SourceManifest::Indexed(manifest) => {
                let mut failure = None;
                crate::index::stream(
                    &Nodes(&self.root),
                    manifest.required_object_index_root.map(child),
                    &mut |entry| {
                        if failure.is_some() {
                            return;
                        }
                        let Some(len) = decode_len(&entry.value) else {
                            failure = Some(Failure::Integrity(Defect::CorruptIndex));
                            return;
                        };
                        if let Err(error) = visit(Object {
                            hash: entry.key,
                            len,
                        }) {
                            failure = Some(error);
                        }
                    },
                )
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?;
                if let Some(error) = failure {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub fn read_object(&self, object: &Object) -> Result<Vec<u8>, Failure> {
        match &self.manifest {
            SourceManifest::Vector(manifest) if !manifest.objects.contains(object) => {
                return Err(Failure::Integrity(Defect::MissingObject));
            }
            SourceManifest::Indexed(manifest) => {
                let value = crate::index::lookup_validated(
                    &Nodes(&self.root),
                    manifest.required_object_index_root.map(child),
                    &object.hash,
                    &|value| decode_len(value).is_some(),
                )
                .map_err(|_| Failure::Integrity(Defect::CorruptIndex))?
                .ok_or(Failure::Integrity(Defect::MissingObject))?;
                if decode_len(&value) != Some(object.len) {
                    return Err(Failure::Integrity(Defect::CorruptObject));
                }
            }
            SourceManifest::Vector(_) => {}
        }
        read_object(&self.root, object)
    }
}

fn validate_object(root: &Path, object: &Object) -> Result<(), Failure> {
    read_object(root, object).map(|_| ())
}

fn read_object(root: &Path, object: &Object) -> Result<Vec<u8>, Failure> {
    crate::v1::read_file_bounded(
        &root.join(OBJECTS_DIR).join(crate::hex(&object.hash)),
        &object.hash,
        object.len,
        object.len,
    )
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
            version: VECTOR_FORMAT_VERSION,
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

    #[test]
    fn streams_the_indexed_v2_source_without_a_required_set_vector() {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("lait-prior-indexed-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(OBJECTS_DIR)).unwrap();
        std::fs::create_dir_all(root.join("journal")).unwrap();
        let payloads = [b"eager control".to_vec(), b"protected payload".to_vec()];
        let objects: Vec<Object> = payloads
            .iter()
            .map(|bytes| Object {
                hash: crate::object_hash(bytes),
                len: u64::try_from(bytes.len()).unwrap(),
            })
            .collect();
        for (object, bytes) in objects.iter().zip(&payloads) {
            std::fs::write(root.join(OBJECTS_DIR).join(crate::hex(&object.hash)), bytes).unwrap();
        }
        let mut sink = crate::index::NodeSink::default();
        let required = crate::index::build_index(
            objects
                .iter()
                .map(|object| crate::index::IndexEntry {
                    key: object.hash,
                    value: object.len.to_be_bytes().to_vec(),
                })
                .collect(),
            &mut sink,
        )
        .unwrap()
        .unwrap();
        for bytes in sink.written {
            let hash = crate::object_hash(&bytes);
            std::fs::write(root.join(OBJECTS_DIR).join(crate::hex(&hash)), bytes).unwrap();
        }
        let meta = b"indexed replica metadata".to_vec();
        let meta_ref = Object {
            hash: crate::object_hash(&meta),
            len: u64::try_from(meta.len()).unwrap(),
        };
        std::fs::write(
            root.join(OBJECTS_DIR).join(crate::hex(&meta_ref.hash)),
            &meta,
        )
        .unwrap();
        let manifest = IndexedManifest {
            format_version: INDEXED_FORMAT_VERSION,
            sequence: 17,
            required_object_index_root: Some((required.hash, required.count)),
            caller_meta: Some(meta_ref),
            caller_index_roots: Vec::new(),
        };
        std::fs::write(
            root.join(MANIFEST_FILE),
            postcard::to_stdvec(&manifest).unwrap(),
        )
        .unwrap();

        let source = Store::open(&root).unwrap();
        assert_eq!(source.sequence(), 17);
        assert_eq!(source.meta(), meta);
        assert_eq!(source.object_count(), 2);
        let mut streamed = Vec::new();
        source
            .for_each_object(|object| {
                streamed.push(source.read_object(&object)?);
                Ok(())
            })
            .unwrap();
        streamed.sort();
        let mut expected = payloads.to_vec();
        expected.sort();
        assert_eq!(streamed, expected);
        let _ = std::fs::remove_dir_all(root);
    }
}
