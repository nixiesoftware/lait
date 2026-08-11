//! Construction of a current Replica generation from committed prior facts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{
    body_index_key, object_ref, receipt_index_key, BodyKey, BodyRecord, CommitContext, Defect,
    Failure, IndexedBody, IndexedReceipt, ManifestRoot, Object, QuotaConfig, Replica,
    ReplicaFrontier, StoreMeta, STORE_META_FORMAT_VERSION,
};
use crate::protected::BodyKeySource;
use crate::receipt::RequestReceipt;

const PRIOR_META_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorMeta {
    version: u8,
    space: Option<mechanics::ids::SpaceId>,
    frontier: ReplicaFrontier,
    quota: QuotaConfig,
    bodies: Vec<(BodyKey, BodyRecord)>,
    receipts: Vec<(Vec<u8>, Object)>,
    manifest_root: Option<Object>,
    manifest_pages: Vec<Object>,
}

/// Evidence that the rebuilt catalogs encode the same committed logical view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verification {
    evidence: [u8; 32],
    bodies: u64,
    receipts: u64,
}

impl Verification {
    pub fn evidence(self) -> [u8; 32] {
        self.evidence
    }

    pub fn bodies(self) -> u64 {
        self.bodies
    }

    pub fn receipts(self) -> u64 {
        self.receipts
    }
}

/// Build and verify a current Replica store from a prior committed store.
/// The source is never modified and the target must be fresh.
pub fn build_prior(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
    context: &CommitContext<'_>,
    keys: Arc<dyn BodyKeySource>,
) -> Result<Verification, Failure> {
    let source = journal::GenerationSource::open(source.as_ref()).map_err(map_journal)?;
    let target_path = target.as_ref().to_path_buf();
    let prior: PriorMeta =
        postcard::from_bytes(source.meta()).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    if prior.version != PRIOR_META_VERSION
        || postcard::to_stdvec(&prior).map_err(|_| Failure::Integrity(Defect::Encoding))?
            != source.meta()
        || prior.space.as_ref() != Some(context.space)
    {
        return Err(Failure::Integrity(Defect::Encoding));
    }

    let mut bodies = BTreeMap::new();
    let mut added = Vec::new();
    let mut required = BTreeSet::new();
    for (key, record) in prior.bodies {
        if record.heads.is_empty() || bodies.insert(key.clone(), record.clone()).is_some() {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        for reference in record_references(&record)? {
            if required.insert(reference.hash) {
                added.push(source.read_object(&reference).map_err(map_journal)?);
            }
        }
    }

    let mut receipts = BTreeMap::new();
    for (scope, reference) in prior.receipts {
        let bytes = source.read_object(&reference).map_err(map_journal)?;
        let receipt = RequestReceipt::decode_canonical(&bytes)
            .map_err(|_| Failure::Integrity(Defect::Encoding))?;
        if receipt.scope_key() != scope
            || receipts.insert(scope, (reference, bytes.clone())).is_some()
        {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        if required.insert(reference.hash) {
            added.push(bytes);
        }
    }

    let evidence = semantic_evidence(
        prior.space.as_ref(),
        prior.frontier,
        prior.quota,
        &bodies,
        &receipts,
    )?;

    let mut sink = crate::index::NodeSink::default();
    let body_entries = bodies
        .iter()
        .map(|(key, record)| {
            let indexed = IndexedBody {
                key: key.clone(),
                record: record.clone(),
            };
            Ok(crate::index::IndexEntry {
                key: body_index_key(key),
                value: postcard::to_stdvec(&indexed)
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?,
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    let body_index_root = crate::index::build_index(body_entries, &mut sink)
        .map_err(|_| Failure::Integrity(Defect::Index))?;

    let published_entries = bodies
        .iter()
        .map(|(key, record)| {
            let entry = Replica::manifest_entry(key, record, Vec::new())?;
            Ok(crate::index::IndexEntry {
                key: body_index_key(key),
                value: entry.encode(),
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    let manifest_body_root = crate::index::build_index(published_entries, &mut sink)
        .map_err(|_| Failure::Integrity(Defect::Index))?;

    let receipt_entries = receipts
        .iter()
        .map(|(scope, (object, _))| {
            let indexed = IndexedReceipt {
                scope: scope.clone(),
                object: *object,
            };
            Ok(crate::index::IndexEntry {
                key: receipt_index_key(scope),
                value: postcard::to_stdvec(&indexed)
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?,
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    let receipt_index_root = crate::index::build_index(receipt_entries, &mut sink)
        .map_err(|_| Failure::Integrity(Defect::Index))?;

    let root = ManifestRoot::sign_with(
        context.space,
        prior.frontier,
        manifest_body_root,
        None,
        context.authority_frontier.clone(),
        context.signer,
    )
    .ok_or_else(|| Failure::Illegitimate("sign rebuilt manifest root".into()))?;
    let root_bytes = root.encode();
    let root_object = object_ref(&root_bytes);
    if required.insert(root_object.hash) {
        added.push(root_bytes);
    }

    let meta = StoreMeta {
        format_version: STORE_META_FORMAT_VERSION,
        space: prior.space,
        frontier: prior.frontier,
        quota: prior.quota,
        body_index_root,
        manifest_body_root,
        content_index_root: None,
        receipt_index_root,
        generation_index_root: None,
        manifest_root: Some(root_object),
    };
    let meta = postcard::to_stdvec(&meta).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    let roots: Vec<([u8; 32], u64)> = body_index_root
        .into_iter()
        .chain(manifest_body_root)
        .chain(receipt_index_root)
        .map(|root| (root.hash, root.count))
        .collect();
    let caller_index = journal::Index {
        roots: &roots,
        nodes: &sink.written,
    };
    let mut target_store = journal::Store::open(&target_path).map_err(map_journal)?;
    if target_store.manifest().is_some() {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    target_store
        .commit(&added, &[], caller_index, meta)
        .map_err(map_journal)?;
    verify_target(&target_store, evidence, &bodies, &receipts)?;
    drop(target_store);

    // Reopening exercises the current store, index, transaction, protected
    // material, receipt, and signed-manifest validators over the result.
    let rebuilt = Replica::open(target_path, keys)?;
    if rebuilt.space.as_ref() != Some(context.space)
        || rebuilt.frontier != prior.frontier
        || rebuilt.bodies.len() != bodies.len()
        || rebuilt.receipts.len() != receipts.len()
    {
        return Err(Failure::Integrity(Defect::Encoding));
    }

    Ok(Verification {
        evidence,
        bodies: u64::try_from(bodies.len()).unwrap_or(u64::MAX),
        receipts: u64::try_from(receipts.len()).unwrap_or(u64::MAX),
    })
}

fn verify_target(
    store: &journal::Store,
    expected_evidence: [u8; 32],
    expected_bodies: &BTreeMap<BodyKey, BodyRecord>,
    expected_receipts: &BTreeMap<Vec<u8>, (Object, Vec<u8>)>,
) -> Result<(), Failure> {
    let meta = store
        .caller_meta()
        .map_err(map_journal)?
        .ok_or(Failure::Integrity(Defect::Encoding))?;
    let meta: StoreMeta =
        postcard::from_bytes(&meta).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    let mut bodies = BTreeMap::new();
    let mut failed = false;
    crate::index::stream(
        &super::StoreNodes(store),
        meta.body_index_root,
        &mut |entry| {
            if failed {
                return;
            }
            match postcard::from_bytes::<IndexedBody>(&entry.value) {
                Ok(indexed) => {
                    if body_index_key(&indexed.key) != entry.key
                        || bodies.insert(indexed.key, indexed.record).is_some()
                    {
                        failed = true;
                    }
                }
                Err(_) => failed = true,
            }
        },
    )
    .map_err(|_| Failure::Integrity(Defect::Index))?;
    if failed || &bodies != expected_bodies {
        return Err(Failure::Integrity(Defect::Encoding));
    }

    let mut receipts = BTreeMap::new();
    crate::index::stream(
        &super::StoreNodes(store),
        meta.receipt_index_root,
        &mut |entry| {
            if failed {
                return;
            }
            let result = postcard::from_bytes::<IndexedReceipt>(&entry.value)
                .ok()
                .filter(|indexed| receipt_index_key(&indexed.scope) == entry.key)
                .and_then(|indexed| {
                    store
                        .read_object(&indexed.object)
                        .ok()
                        .map(|bytes| (indexed, bytes))
                });
            match result {
                Some((indexed, bytes)) => {
                    if receipts
                        .insert(indexed.scope, (indexed.object, bytes))
                        .is_some()
                    {
                        failed = true;
                    }
                }
                None => failed = true,
            }
        },
    )
    .map_err(|_| Failure::Integrity(Defect::Index))?;
    let actual = semantic_evidence(
        meta.space.as_ref(),
        meta.frontier,
        meta.quota,
        &bodies,
        &receipts,
    )?;
    if failed || &receipts != expected_receipts || actual != expected_evidence {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    Ok(())
}

fn record_references(record: &BodyRecord) -> Result<Vec<Object>, Failure> {
    let mut references = Vec::with_capacity(record.heads.len().saturating_mul(2));
    for head in &record.heads {
        let protected = head
            .protected
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        let transaction = head
            .transaction
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        references.push(protected);
        references.push(transaction);
    }
    Ok(references)
}

fn semantic_evidence(
    space: Option<&mechanics::ids::SpaceId>,
    frontier: ReplicaFrontier,
    quota: QuotaConfig,
    bodies: &BTreeMap<BodyKey, BodyRecord>,
    receipts: &BTreeMap<Vec<u8>, (Object, Vec<u8>)>,
) -> Result<[u8; 32], Failure> {
    let bytes = postcard::to_stdvec(&(space, frontier, quota, bodies, receipts))
        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/replica-generation/1/equivalence");
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}

fn map_journal(failure: journal::Failure) -> Failure {
    match failure {
        journal::Failure::Integrity(defect) => Failure::Integrity(Defect::Store(defect)),
        journal::Failure::OutcomeUnknown => Failure::OutcomeUnknown,
        other => Failure::Durability(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontier::AuthorityFrontier;
    use crate::transaction::SeedSigner;
    use mechanics::authorization::AuthorizedBodyKey;
    use mechanics::ids::SpaceId;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn directory(name: &str) -> std::path::PathBuf {
        let next = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "lait-replica-generation-{name}-{}-{next}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("objects")).unwrap();
        std::fs::create_dir_all(path.join("journal")).unwrap();
        path
    }

    #[derive(Serialize)]
    struct PriorManifest {
        version: u8,
        sequence: u64,
        objects: Vec<Object>,
        meta: Vec<u8>,
    }

    struct NoKeys;

    impl BodyKeySource for NoKeys {
        fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
            None
        }

        fn opening_key(&self, _epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
            None
        }
    }

    #[test]
    fn an_empty_prior_replica_becomes_a_verified_current_store() {
        let source = directory("source");
        let target = directory("target");
        let space = SpaceId::from_digest([41; 16]);
        let prior = PriorMeta {
            version: PRIOR_META_VERSION,
            space: Some(space.clone()),
            frontier: ReplicaFrontier::EMPTY,
            quota: QuotaConfig::default(),
            bodies: Vec::new(),
            receipts: Vec::new(),
            manifest_root: None,
            manifest_pages: Vec::new(),
        };
        let manifest = PriorManifest {
            version: 1,
            sequence: 4,
            objects: Vec::new(),
            meta: postcard::to_stdvec(&prior).unwrap(),
        };
        std::fs::write(
            source.join("current-manifest"),
            postcard::to_stdvec(&manifest).unwrap(),
        )
        .unwrap();

        let seed = [9; 32];
        let signer = SeedSigner(&seed);
        let context = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: AuthorityFrontier::from_canonical_bytes(Vec::new()),
        };
        let verified = build_prior(&source, &target, &context, Arc::new(NoKeys)).unwrap();
        assert_eq!(verified.bodies(), 0);
        assert_eq!(verified.receipts(), 0);
        assert_ne!(verified.evidence(), [0; 32]);
        let store = journal::Store::open(&target).unwrap();
        assert_eq!(
            store.manifest().unwrap().format_version,
            journal::STORE_FORMAT_VERSION
        );
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(target);
    }
}
