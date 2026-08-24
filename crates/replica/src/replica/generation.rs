//! Construction of a current Replica generation from committed prior facts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{
    advance, advance_chain, body_index_key, fabric_key, lock_fabric, object_ref,
    ownership_index_key, receipt_index_key, validate_receipt_for_storage, ActionOutcome, BodyHead,
    BodyKey, BodyRecord, CommitAuthorization, CommitContext, Defect, Failure, IndexedBody,
    IndexedOwnership, IndexedReceipt, ManifestRoot, Object, OwnedObjectClass,
    PriorIndexedStoreMeta, QuotaConfig, Replica, ReplicaFrontier, SignRequest, StoreMeta,
    Transaction, STORE_META_FORMAT_VERSION,
};
use crate::protected::BodyKeySource;
use crate::receipt::{Interpretation, RequestReceipt};
use crate::transaction::{AuthoritySource, Core, TransactionAuthorizer};

const PRIOR_META_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorBodyHead {
    tx: [u8; 32],
    descriptor_hash: [u8; 32],
    tx_commitment: [u8; 32],
    protected: Option<Object>,
    transaction: Option<Object>,
    protected_len: u64,
    tx_len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorBodyRecord {
    binding: super::BodyBinding,
    chain: ReplicaFrontier,
    heads: Vec<PriorBodyHead>,
    interpreted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorMeta {
    version: u8,
    space: Option<mechanics::ids::SpaceId>,
    frontier: ReplicaFrontier,
    quota: QuotaConfig,
    bodies: Vec<(BodyKey, PriorBodyRecord)>,
    receipts: Vec<(Vec<u8>, Object)>,
    manifest_root: Option<Object>,
    manifest_pages: Vec<Object>,
}

/// One canonical whole-Body value opened from a prior signed head. These bytes
/// are migration input only; they are never installed as a current BodyHead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorOpenedMaterial {
    pub epoch: [u8; 16],
    pub payload: fabric::BodyExport,
    pub base_frontier: ReplicaFrontier,
    pub resulting_frontier: ReplicaFrontier,
}

/// Signed transaction coordinates retained as evidence while composition
/// authors the replacement current transaction under update consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorTransactionEvidence {
    pub id: [u8; 32],
    pub bytes: Arc<[u8]>,
    pub parent_manifest_root: [u8; 32],
    pub replica_frontier: ReplicaFrontier,
    pub authority_frontier: crate::frontier::AuthorityFrontier,
    pub actor: String,
    pub signer: [u8; 32],
    pub intent_digest: [u8; 32],
    pub operations_digest: [u8; 32],
    pub demand: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorHeadEvidence {
    pub descriptor_hash: [u8; 32],
    pub transaction_commitment: [u8; 32],
    pub transaction: PriorTransactionEvidence,
    pub material: Option<PriorOpenedMaterial>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorBodyEvidence {
    pub key: BodyKey,
    pub binding: super::BodyBinding,
    pub chain: ReplicaFrontier,
    pub interpreted: bool,
    pub heads: Vec<PriorHeadEvidence>,
    pub content_refs: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorBodyPage {
    pub bodies: Vec<PriorBodyEvidence>,
    /// Exclusive prior-index cursor. `None` means this was the final page.
    pub next: Option<[u8; 32]>,
}

/// A canonical prior request receipt. Publication digests did not exist in
/// this generation; the composition migrator supplies them from the reviewed
/// target publication rather than inventing historical values here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorReceiptEvidence {
    pub version: u8,
    pub space: mechanics::ids::SpaceId,
    pub world: crate::body::WorldId,
    pub device: mechanics::ids::DeviceId,
    pub request: [u8; 16],
    pub payload_hash: [u8; 32],
    pub effect: Vec<u8>,
    pub bodies: Vec<BodyKey>,
    pub frontier: ReplicaFrontier,
    pub transaction: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorReceiptPage {
    pub receipts: Vec<(Vec<u8>, Arc<[u8]>, PriorReceiptEvidence)>,
    /// Exclusive prior-index cursor. `None` means this was the final page.
    pub next: Option<[u8; 32]>,
}

/// The exact signed prior catalog coordinate. Composition verifies the
/// signer's standing at `authority_frontier` before replaying any page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorManifestEvidence {
    pub bytes: Arc<[u8]>,
    pub space: mechanics::ids::SpaceId,
    pub frontier: ReplicaFrontier,
    pub body_index_root: Option<([u8; 32], u64)>,
    pub content_index_root: Option<([u8; 32], u64)>,
    pub signer: [u8; 32],
    pub authority_frontier: crate::frontier::AuthorityFrontier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorContentPage {
    pub descriptors: Vec<crate::content::ContentDescriptor>,
    pub next: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyDescriptor {
    world: crate::body::WorldId,
    body: crate::body::BodyId,
    schema: crate::body::SchemaId,
    schema_version: u32,
    encoding: crate::body::EncodingId,
    content_commitment: [u8; 32],
}

impl LegacyDescriptor {
    fn key(&self) -> BodyKey {
        BodyKey::new(self.world.clone(), self.body.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyCore {
    version: u8,
    space: [u8; 29],
    parent_manifest_root: [u8; 32],
    replica_frontier: ReplicaFrontier,
    authority_frontier: crate::frontier::AuthorityFrontier,
    actor: String,
    signer: [u8; 32],
    intent_digest: [u8; 32],
    operations_digest: [u8; 32],
    demand: Vec<u8>,
    descriptors: Vec<LegacyDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyTransaction {
    core: LegacyCore,
    authorization_receipt: Vec<u8>,
    signature_algorithm: u8,
    #[serde(with = "serde_byte_array")]
    signature: [u8; 64],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyMaterial {
    version: u8,
    mutation_model: u8,
    payload: fabric::BodyExport,
    base_frontier: ReplicaFrontier,
    resulting_frontier: ReplicaFrontier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyIndexedBody {
    key: BodyKey,
    record: PriorBodyRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyIndexedReceipt {
    scope: Vec<u8>,
    object: Object,
}

const LEGACY_TRANSACTION_DOMAIN: &[u8] = b"lait/body-transaction/2";
const LEGACY_CORE_DIGEST_CONTEXT: &str = "lait.body-transaction-core.v1";
const LEGACY_TRANSACTION_ID_CONTEXT: &str = "lait.body-transaction-id.v1";
const LEGACY_MAX_TRANSACTION: usize = 1024 * 1024;
const LEGACY_MAX_DESCRIPTORS: usize = 4096;
const LEGACY_MAX_MATERIAL: usize = 64 * 1024 * 1024;

fn legacy_length_framed(domain: &[u8], body: &[u8]) -> Option<Vec<u8>> {
    let domain_len = u16::try_from(domain.len()).ok()?;
    let body_len = u32::try_from(body.len()).ok()?;
    let mut out = Vec::with_capacity(
        2usize
            .checked_add(domain.len())?
            .checked_add(4)?
            .checked_add(body.len())?,
    );
    out.extend_from_slice(&domain_len.to_be_bytes());
    out.extend_from_slice(domain);
    out.extend_from_slice(&body_len.to_be_bytes());
    out.extend_from_slice(body);
    Some(out)
}

fn legacy_core_digest(core: &LegacyCore) -> Result<[u8; 32], Failure> {
    let bytes = postcard::to_stdvec(core).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    Ok(blake3::derive_key(LEGACY_CORE_DIGEST_CONTEXT, &bytes))
}

fn legacy_transaction_id(bytes: &[u8]) -> [u8; 32] {
    blake3::derive_key(LEGACY_TRANSACTION_ID_CONTEXT, bytes)
}

fn decode_legacy_transaction(bytes: &[u8]) -> Result<LegacyTransaction, Failure> {
    if bytes.len() > LEGACY_MAX_TRANSACTION {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    let tx: LegacyTransaction =
        postcard::from_bytes(bytes).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    if postcard::to_stdvec(&tx).ok().as_deref() != Some(bytes)
        || tx.core.version != 1
        || tx.signature_algorithm != 1
        || tx.core.descriptors.is_empty()
        || tx.core.descriptors.len() > LEGACY_MAX_DESCRIPTORS
    {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    let space = std::str::from_utf8(&tx.core.space)
        .ok()
        .and_then(mechanics::ids::SpaceId::parse)
        .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
    if mechanics::ids::ActorId::parse(&tx.core.actor).is_none() {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    if tx
        .core
        .descriptors
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left.key() >= right.key()))
    {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    let demand = mechanics::authorization::AuthorizationDemand::decode_canonical(&tx.core.demand)
        .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
    let receipt = mechanics::authorization::AuthorizationReceipt::decode(&tx.authorization_receipt)
        .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
    if receipt.space != space.as_str()
        || receipt.actor != tx.core.actor
        || receipt.device != tx.core.signer
        || receipt.authority_frontier != tx.core.authority_frontier.as_bytes()
        || receipt.parent_manifest_root != tx.core.parent_manifest_root
        || receipt.intent_digest != tx.core.intent_digest
        || receipt.effect_operations_digest != tx.core.operations_digest
        || receipt.demand_digest
            != demand
                .digest()
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?
        || receipt.body_transaction_core_digest != legacy_core_digest(&tx.core)?
    {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    let preimage_body = postcard::to_stdvec(&(&tx.core, &tx.authorization_receipt))
        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
    let preimage = legacy_length_framed(LEGACY_TRANSACTION_DOMAIN, &preimage_body)
        .ok_or(Failure::Integrity(Defect::Encoding))?;
    if !mechanics::actor::verify_detached(&tx.core.signer, &preimage, &tx.signature) {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    Ok(tx)
}

fn open_legacy_material(
    opening: &mechanics::authorization::AuthorizedBodyKey,
    envelope: &[u8],
    record: &PriorBodyRecord,
) -> Result<PriorOpenedMaterial, Failure> {
    if envelope.len() > LEGACY_MAX_MATERIAL {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    let epoch = mechanics::authorization::body_epoch_id(envelope)
        .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
    if opening.epoch_id() != &epoch {
        return Err(Failure::BodyKeyUnavailable);
    }
    let bytes = mechanics::authorization::body_open(opening, envelope)
        .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
    let material: LegacyMaterial =
        postcard::from_bytes(&bytes).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    if postcard::to_stdvec(&material).ok().as_deref() != Some(bytes.as_slice())
        || material.version != 1
        || material.mutation_model != record.binding.mutation_model
        || match (&material.payload, material.mutation_model) {
            (fabric::BodyExport::Atomic(_), super::MUTATION_ATOMIC) => false,
            (fabric::BodyExport::Collaborative(_), super::MUTATION_COLLABORATIVE) => false,
            _ => true,
        }
        || material.resulting_frontier.transaction_count
            != material.base_frontier.transaction_count.saturating_add(1)
    {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    Ok(PriorOpenedMaterial {
        epoch,
        payload: material.payload,
        base_frontier: material.base_frontier,
        resulting_frontier: material.resulting_frontier,
    })
}

fn validate_prior_advertisement(
    key: &BodyKey,
    record: &PriorBodyRecord,
    advertised: &crate::manifest::ManifestEntry,
) -> Result<(), Failure> {
    let expected_heads = record
        .heads
        .iter()
        .map(|head| crate::manifest::ManifestHead {
            descriptor_hash: head.descriptor_hash,
            transaction_commitment: head.tx_commitment,
        })
        .collect::<Vec<_>>();
    if advertised.key != *key || advertised.heads != expected_heads {
        return Err(Failure::Integrity(Defect::Index));
    }
    Ok(())
}

/// Validated, read-only streaming access to an actual indexed Journal-v2
/// Replica. The source directory remains untouched. A caller may build a fresh
/// target, verify it, and activate it atomically, but cannot reinterpret these
/// old signatures as current causal descriptors.
pub struct PriorReplicaSource {
    source: journal::GenerationSource,
    meta: PriorIndexedStoreMeta,
    manifest: PriorManifestEvidence,
    keys: Arc<dyn BodyKeySource>,
}

/// Decode an indexed prior store's caller metadata, from either generation
/// that carries one.
///
/// Version 3 differs only by a generation index root, which a rebuild derives
/// fresh, so it normalizes onto the version-2 shape the migration reads. Both
/// are required to be canonical.
fn decode_prior_indexed_meta(bytes: &[u8]) -> Result<PriorIndexedStoreMeta, Failure> {
    if let Ok(meta) = postcard::from_bytes::<super::PriorGenerationStoreMeta>(bytes) {
        if meta.format_version == 3
            && postcard::to_stdvec(&meta).map_err(|_| Failure::Integrity(Defect::Encoding))?
                == bytes
        {
            return Ok(PriorIndexedStoreMeta {
                format_version: 2,
                space: meta.space,
                frontier: meta.frontier,
                quota: meta.quota,
                body_index_root: meta.body_index_root,
                manifest_body_root: meta.manifest_body_root,
                content_index_root: meta.content_index_root,
                receipt_index_root: meta.receipt_index_root,
                manifest_root: meta.manifest_root,
            });
        }
    }
    let meta: PriorIndexedStoreMeta =
        postcard::from_bytes(bytes).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    if meta.format_version != 2
        || postcard::to_stdvec(&meta).map_err(|_| Failure::Integrity(Defect::Encoding))? != bytes
    {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    Ok(meta)
}

impl PriorReplicaSource {
    pub fn open(path: impl AsRef<Path>, keys: Arc<dyn BodyKeySource>) -> Result<Self, Failure> {
        let source = journal::GenerationSource::open(path.as_ref()).map_err(map_journal)?;
        let meta = decode_prior_indexed_meta(source.meta())?;
        let committed: BTreeSet<([u8; 32], u64)> =
            source.caller_index_roots().into_iter().collect();
        for root in [
            meta.body_index_root,
            meta.manifest_body_root,
            meta.content_index_root,
            meta.receipt_index_root,
        ]
        .into_iter()
        .flatten()
        {
            if !committed.contains(&(root.hash, root.count)) {
                return Err(Failure::Integrity(Defect::Index));
            }
        }
        let root_ref = meta
            .manifest_root
            .ok_or(Failure::Integrity(Defect::Encoding))?;
        let root_bytes = source.read_object(&root_ref).map_err(map_journal)?;
        let root = crate::manifest::ManifestRoot::decode_canonical(&root_bytes)
            .map_err(|_| Failure::Integrity(Defect::Encoding))?;
        root.verify()
            .map_err(|_| Failure::Integrity(Defect::Encoding))?;
        let space = std::str::from_utf8(&root.space)
            .ok()
            .and_then(mechanics::ids::SpaceId::parse)
            .ok_or(Failure::Integrity(Defect::Encoding))?;
        if meta.space.as_ref() != Some(&space)
            || root.replica_frontier != meta.frontier
            || root.body_index_root != meta.manifest_body_root
            || root.content_index_root != meta.content_index_root
            || root.body_count != meta.body_index_root.map_or(0, |child| child.count)
        {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        let manifest = PriorManifestEvidence {
            bytes: Arc::from(root_bytes),
            space,
            frontier: root.replica_frontier,
            body_index_root: root.body_index_root.map(|child| (child.hash, child.count)),
            content_index_root: root
                .content_index_root
                .map(|child| (child.hash, child.count)),
            signer: root.signer,
            authority_frontier: root.authority_frontier,
        };
        Ok(Self {
            source,
            meta,
            manifest,
            keys,
        })
    }

    pub fn space(&self) -> Option<&mechanics::ids::SpaceId> {
        self.meta.space.as_ref()
    }

    pub fn frontier(&self) -> ReplicaFrontier {
        self.meta.frontier
    }

    pub fn quota(&self) -> QuotaConfig {
        self.meta.quota
    }

    pub fn manifest(&self) -> &PriorManifestEvidence {
        &self.manifest
    }

    pub fn body_count(&self) -> u64 {
        self.meta.body_index_root.map_or(0, |root| root.count)
    }

    pub fn receipt_count(&self) -> u64 {
        self.meta.receipt_index_root.map_or(0, |root| root.count)
    }

    pub fn content_count(&self) -> u64 {
        self.meta.content_index_root.map_or(0, |root| root.count)
    }

    pub fn for_each_body(
        &self,
        mut visit: impl FnMut(PriorBodyEvidence) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let mut after = None;
        loop {
            let page = self.body_page(after, 4096)?;
            for body in page.bodies {
                visit(body)?;
            }
            let Some(next) = page.next else { return Ok(()) };
            after = Some(next);
        }
    }

    pub fn body_page(&self, after: Option<[u8; 32]>, limit: u16) -> Result<PriorBodyPage, Failure> {
        let Some(root) = self.meta.body_index_root else {
            return Ok(PriorBodyPage {
                bodies: Vec::new(),
                next: None,
            });
        };
        let page = self
            .source
            .caller_index_page((root.hash, root.count), after, limit)
            .map_err(map_journal)?;
        let mut bodies = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let indexed: LegacyIndexedBody = postcard::from_bytes(&entry.value)
                .map_err(|_| Failure::Integrity(Defect::Index))?;
            if postcard::to_stdvec(&indexed).ok().as_deref() != Some(entry.value.as_slice())
                || body_index_key(&indexed.key) != entry.key
                || indexed.record.heads.is_empty()
            {
                return Err(Failure::Integrity(Defect::Index));
            }
            bodies.push(self.open_body(indexed)?);
        }
        Ok(PriorBodyPage {
            bodies,
            next: page.next,
        })
    }

    pub fn for_each_receipt(
        &self,
        mut visit: impl FnMut(Vec<u8>, Arc<[u8]>, PriorReceiptEvidence) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let mut after = None;
        loop {
            let page = self.receipt_page(after, 4096)?;
            for (scope, bytes, receipt) in page.receipts {
                visit(scope, bytes, receipt)?;
            }
            let Some(next) = page.next else { return Ok(()) };
            after = Some(next);
        }
    }

    pub fn receipt_page(
        &self,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> Result<PriorReceiptPage, Failure> {
        let Some(root) = self.meta.receipt_index_root else {
            return Ok(PriorReceiptPage {
                receipts: Vec::new(),
                next: None,
            });
        };
        let page = self
            .source
            .caller_index_page((root.hash, root.count), after, limit)
            .map_err(map_journal)?;
        let mut receipts = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let indexed: LegacyIndexedReceipt = postcard::from_bytes(&entry.value)
                .map_err(|_| Failure::Integrity(Defect::Index))?;
            if postcard::to_stdvec(&indexed).ok().as_deref() != Some(entry.value.as_slice())
                || receipt_index_key(&indexed.scope) != entry.key
            {
                return Err(Failure::Integrity(Defect::Index));
            }
            let bytes = self
                .source
                .read_object(&indexed.object)
                .map_err(map_journal)?;
            let receipt: PriorReceiptEvidence =
                postcard::from_bytes(&bytes).map_err(|_| Failure::Integrity(Defect::Encoding))?;
            if postcard::to_stdvec(&receipt).ok().as_deref() != Some(bytes.as_slice())
                || receipt.version != 1
                || receipt.effect.len() > crate::receipt::MAX_EFFECT_BYTES
                || crate::receipt::scope_key(
                    &receipt.space,
                    &receipt.world,
                    &receipt.device,
                    &receipt.request,
                ) != indexed.scope
            {
                return Err(Failure::Integrity(Defect::Encoding));
            }
            receipts.push((indexed.scope, Arc::from(bytes), receipt));
        }
        Ok(PriorReceiptPage {
            receipts,
            next: page.next,
        })
    }

    pub fn content_page(
        &self,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> Result<PriorContentPage, Failure> {
        let Some(root) = self.meta.content_index_root else {
            return Ok(PriorContentPage {
                descriptors: Vec::new(),
                next: None,
            });
        };
        let page = self
            .source
            .caller_index_page((root.hash, root.count), after, limit)
            .map_err(map_journal)?;
        let mut descriptors = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let descriptor = crate::content::ContentDescriptor::decode_canonical(&entry.value)
                .map_err(|_| Failure::Integrity(Defect::Encoding))?;
            if descriptor.space != self.manifest.space.as_str()
                || crate::manifest::content_index_key(descriptor.content_ref().as_bytes())
                    != entry.key
            {
                return Err(Failure::Integrity(Defect::Index));
            }
            descriptors.push(descriptor);
        }
        Ok(PriorContentPage {
            descriptors,
            next: page.next,
        })
    }

    fn open_body(&self, indexed: LegacyIndexedBody) -> Result<PriorBodyEvidence, Failure> {
        let LegacyIndexedBody { key, record } = indexed;
        let manifest_root = self
            .meta
            .manifest_body_root
            .ok_or(Failure::Integrity(Defect::Index))?;
        let manifest_bytes = self
            .source
            .caller_index_lookup(
                (manifest_root.hash, manifest_root.count),
                &body_index_key(&key),
            )
            .map_err(map_journal)?
            .ok_or(Failure::Integrity(Defect::Index))?;
        let advertised = crate::manifest::ManifestEntry::decode_canonical(&manifest_bytes)
            .map_err(|_| Failure::Integrity(Defect::Encoding))?;
        validate_prior_advertisement(&key, &record, &advertised)?;
        for content in &advertised.content_refs {
            let root = self
                .meta
                .content_index_root
                .ok_or(Failure::Integrity(Defect::Index))?;
            let bytes = self
                .source
                .caller_index_lookup(
                    (root.hash, root.count),
                    &crate::manifest::content_index_key(content),
                )
                .map_err(map_journal)?
                .ok_or(Failure::Integrity(Defect::Index))?;
            let descriptor = crate::content::ContentDescriptor::decode_canonical(&bytes)
                .map_err(|_| Failure::Integrity(Defect::Encoding))?;
            if descriptor.space != self.manifest.space.as_str()
                || descriptor.content_ref().as_bytes() != content
            {
                return Err(Failure::Integrity(Defect::Index));
            }
        }
        let mut heads = Vec::with_capacity(record.heads.len());
        for head in &record.heads {
            let protected = head
                .protected
                .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            let transaction_ref = head
                .transaction
                .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            if protected.len != head.protected_len || transaction_ref.len != head.tx_len {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
            let transaction_bytes = self
                .source
                .read_object(&transaction_ref)
                .map_err(map_journal)?;
            let transaction = decode_legacy_transaction(&transaction_bytes)?;
            if legacy_transaction_id(&transaction_bytes) != head.tx
                || *blake3::hash(&transaction_bytes).as_bytes() != head.tx_commitment
            {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
            let descriptor = transaction
                .core
                .descriptors
                .iter()
                .find(|descriptor| descriptor.key() == key)
                .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            let descriptor_bytes = postcard::to_stdvec(descriptor)
                .map_err(|_| Failure::Integrity(Defect::Encoding))?;
            if *blake3::hash(&descriptor_bytes).as_bytes() != head.descriptor_hash
                || descriptor.schema != record.binding.schema
                || descriptor.schema_version != record.binding.schema_version
                || descriptor.encoding != record.binding.encoding
            {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
            let envelope = self.source.read_object(&protected).map_err(map_journal)?;
            if crate::body::ContentCommitment::over_protected_payload(&envelope).as_bytes()
                != descriptor.content_commitment
            {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
            let epoch = mechanics::authorization::body_epoch_id(&envelope)
                .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
            let material = match self.keys.opening_key(&epoch) {
                Some(opening) => Some(open_legacy_material(&opening, &envelope, &record)?),
                None if record.interpreted => return Err(Failure::BodyKeyUnavailable),
                None => None,
            };
            heads.push(PriorHeadEvidence {
                descriptor_hash: head.descriptor_hash,
                transaction_commitment: head.tx_commitment,
                transaction: PriorTransactionEvidence {
                    id: head.tx,
                    bytes: Arc::from(transaction_bytes),
                    parent_manifest_root: transaction.core.parent_manifest_root,
                    replica_frontier: transaction.core.replica_frontier,
                    authority_frontier: transaction.core.authority_frontier.clone(),
                    actor: transaction.core.actor.clone(),
                    signer: transaction.core.signer,
                    intent_digest: transaction.core.intent_digest,
                    operations_digest: transaction.core.operations_digest,
                    demand: transaction.core.demand.clone(),
                },
                material,
            });
        }
        if record.interpreted {
            let mut derived = None;
            for head in &heads {
                let material = head
                    .material
                    .as_ref()
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                derived = Some(match (&material.payload, derived) {
                    (_, None) => material.resulting_frontier,
                    (fabric::BodyExport::Atomic(_), Some(current)) => {
                        if super::chain_order(&material.resulting_frontier, &current).is_gt() {
                            material.resulting_frontier
                        } else {
                            current
                        }
                    }
                    (fabric::BodyExport::Collaborative(_), Some(current)) => {
                        super::combine_chains(&current, &material.resulting_frontier)
                    }
                });
            }
            if derived != Some(record.chain) {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
        }
        Ok(PriorBodyEvidence {
            key,
            binding: record.binding,
            chain: record.chain,
            interpreted: record.interpreted,
            heads,
            content_refs: advertised.content_refs,
        })
    }
}

const SEMANTIC_MIGRATION_BATCH: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorMigrationEffect {
    version: u8,
    source_manifest: [u8; 32],
    world: crate::body::WorldId,
    bodies: Vec<BodyKey>,
}

struct MigrationAuthorizer<'a, F> {
    world: &'a crate::body::WorldId,
    authorize: &'a F,
}

impl<F> TransactionAuthorizer for MigrationAuthorizer<'_, F>
where
    F: Fn(&crate::body::WorldId, &Core) -> Result<Vec<u8>, mechanics::authorization::Refusal>,
{
    fn authorize(&self, core: &Core) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        (self.authorize)(self.world, core)
    }
}

fn prior_body_snapshot(body: &PriorBodyEvidence) -> Result<fabric::BodySnapshot, Failure> {
    if !body.interpreted || body.heads.iter().any(|head| head.material.is_none()) {
        return Err(Failure::BodyKeyUnavailable);
    }
    let mut engine = fabric::Engine::new();
    for head in &body.heads {
        let material = head.material.as_ref().ok_or(Failure::BodyKeyUnavailable)?;
        let status = engine
            .import_body(&fabric_key(&body.key), &material.payload)
            .map_err(Failure::Engine)?;
        if status
            .as_ref()
            .is_some_and(|receipt| receipt.applied() == 0)
        {
            continue;
        }
    }
    engine
        .body_snapshot(&fabric_key(&body.key))
        .map_err(Failure::Engine)?
        .ok_or(Failure::Integrity(Defect::MissingMaterial))
}

fn migration_digest(
    source_manifest: &[u8; 32],
    rows: &[(PriorBodyEvidence, fabric::BodySnapshot)],
) -> Result<[u8; 32], Failure> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/prior-semantic-migration/1");
    hash.update(source_manifest);
    for (body, snapshot) in rows {
        let coordinates = postcard::to_stdvec(&(&body.key, &body.binding, &body.chain))
            .map_err(|_| Failure::Integrity(Defect::Encoding))?;
        hash.update(
            &u64::try_from(coordinates.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hash.update(&coordinates);
        let bytes = snapshot.canonical_export_shared();
        hash.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(&bytes);
        for reference in &body.content_refs {
            hash.update(reference);
        }
    }
    Ok(*hash.finalize().as_bytes())
}

impl Replica {
    /// Author one bounded current transaction from verified prior signed
    /// whole-Body evidence. This is the sole representation bridge: it accepts
    /// no World operations and only the evidence type constructed by
    /// [`PriorReplicaSource`].
    #[allow(clippy::too_many_arguments)]
    fn commit_prior_batch(
        &mut self,
        ctx: &CommitContext<'_>,
        auth: &CommitAuthorization<'_>,
        world: &crate::body::WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        digest: &[u8; 32],
        effect: Vec<u8>,
        rows: &[(PriorBodyEvidence, fabric::BodySnapshot)],
    ) -> Result<ActionOutcome, Failure> {
        self.mutation_available()?;
        if let Some(receipt) = self.lookup_action(ctx.space, world, device, request, digest)? {
            return Ok(ActionOutcome::Replayed(receipt));
        }
        if rows.is_empty()
            || rows.iter().any(|(body, _)| &body.key.world != world)
            || rows
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left.0.key >= right.0.key))
            || rows
                .iter()
                .any(|(body, _)| self.bodies.contains_key(&body.key))
        {
            return Err(Failure::Illegitimate(
                "prior migration batch is not a fresh, ordered single-World set".into(),
            ));
        }
        if effect.len() > crate::receipt::MAX_EFFECT_BYTES {
            return Err(Failure::EffectTooLarge);
        }
        match &self.space {
            None => self.space = Some(ctx.space.clone()),
            Some(space) if space == ctx.space => {}
            Some(_) => {
                return Err(Failure::Illegitimate(
                    "prior migration addressed to a different Space".into(),
                ));
            }
        }
        if u64::try_from(self.bodies.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX))
            > self.quota.max_space_bodies
        {
            return Err(Failure::QuotaExceeded);
        }
        if self
            .keys
            .as_ref()
            .and_then(|keys| keys.sealing_key())
            .is_none()
        {
            return Err(Failure::BodyKeyUnavailable);
        }

        let snapshots = rows
            .iter()
            .map(|(body, snapshot)| (fabric_key(&body.key), snapshot.clone()))
            .collect::<Vec<_>>();
        let fabric = lock_fabric(&self.fabric)
            .prepare_verified_snapshots(&snapshots)
            .map_err(Failure::Engine)?;
        let assembled = (|| -> Result<super::PreparedMutation, Failure> {
            let next_frontier = advance(self.frontier, fabric.receipt().causal().as_bytes());
            let chain_seed = super::mint_chain_seed()?;
            let mut new_records = BTreeMap::new();
            let mut sealed = Vec::new();
            let mut new_artifacts = Vec::new();
            let mut declared = BTreeMap::new();
            for (body, _) in rows {
                let mut record = BodyRecord {
                    binding: body.binding.clone(),
                    chain: advance_chain(ReplicaFrontier::EMPTY, &chain_seed),
                    heads: smallvec::smallvec![BodyHead {
                        tx: [0u8; 32],
                        descriptor_hash: [0u8; 32],
                        tx_commitment: [0u8; 32],
                        artifacts: Some(Vec::new().into_boxed_slice()),
                        transaction: None,
                        artifact_bytes: 0,
                        tx_len: 0,
                    }],
                    causal: None,
                    interpreted: true,
                };
                let (material, artifacts) =
                    self.next_causal_material(&body.key, &record, None, &new_artifacts)?;
                let pack = super::encode_artifact_pack(&artifacts)?;
                new_artifacts.extend(artifacts);
                record.causal = Some(Arc::new(material.clone()));
                sealed.push((body.key.clone(), pack, material));
                let mut refs = body.content_refs.clone();
                refs.sort_unstable();
                refs.dedup();
                declared.insert(body.key.clone(), refs);
                new_records.insert(body.key.clone(), Some(record));
            }

            let mut descriptors = Vec::with_capacity(sealed.len());
            for (key, _, material) in &sealed {
                let record = new_records
                    .get(key)
                    .and_then(Option::as_ref)
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                descriptors.push(crate::transaction::Descriptor {
                    world: key.world.clone(),
                    body: key.body.clone(),
                    schema: record.binding.schema.clone(),
                    schema_version: record.binding.schema_version,
                    encoding: record.binding.encoding.clone(),
                    mutation_model: record.binding.mutation_model,
                    base_frontier: ReplicaFrontier::EMPTY,
                    resulting_frontier: record.chain,
                    material: material.clone(),
                });
            }
            descriptors.sort_by_key(crate::transaction::Descriptor::key);
            let transaction = Transaction::sign_with(
                SignRequest {
                    space: ctx.space,
                    parent_manifest_root: auth.parent_manifest_root,
                    replica_frontier: next_frontier,
                    authority_frontier: ctx.authority_frontier.clone(),
                    actor: auth.actor,
                    operation: *request,
                    intent_digest: auth.intent_digest,
                    operations_digest: *digest,
                    demand: auth.demand.clone(),
                    descriptors,
                },
                ctx.signer,
                |core| auth.authorizer.authorize(core),
            )
            .map_err(Failure::Unauthorized)?;
            if transaction.encode().len() > crate::transaction::MAX_TRANSACTION {
                return Err(Failure::OpLimit);
            }
            let tx_id = transaction.id();
            for record in new_records.values_mut().flatten() {
                record.head_mut()?.tx = tx_id;
            }
            Self::populate_local_record_refs(
                &transaction,
                &sealed,
                &mut new_records,
                self.durable.is_some(),
            )?;

            let keys = rows
                .iter()
                .map(|(body, _)| body.key.clone())
                .collect::<Vec<_>>();
            let mut receipt = RequestReceipt {
                version: 2,
                space: ctx.space.clone(),
                world: world.clone(),
                device: device.clone(),
                request: *request,
                payload_hash: *digest,
                effect,
                bodies: keys,
                frontier: next_frontier,
                manifest_root: [0u8; 32],
                implementation_digest: Interpretation::UNSPECIFIED.implementation_digest,
                extractor_schema_digest: Interpretation::UNSPECIFIED.extractor_schema_digest,
                transaction: tx_id,
            };
            let receipt_bytes = validate_receipt_for_storage(&receipt)?;
            let (mut projected, _) = self.usage();
            for (key, pack, _) in &sealed {
                let artifact_len = new_records
                    .get(key)
                    .and_then(Option::as_ref)
                    .map(BodyRecord::protected_total)
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                if artifact_len > self.quota.max_body_bytes
                    || u64::try_from(pack.len()).unwrap_or(u64::MAX) > self.quota.max_body_bytes
                {
                    return Err(Failure::QuotaExceeded);
                }
                projected = projected.saturating_add(artifact_len);
            }
            projected = projected
                .saturating_add(u64::try_from(transaction.encode().len()).unwrap_or(u64::MAX))
                .saturating_add(u64::try_from(receipt_bytes.len()).unwrap_or(u64::MAX));
            if projected > self.quota.max_space_bytes {
                return Err(Failure::QuotaExceeded);
            }
            let candidate_root =
                self.preview_manifest_root(ctx, &new_records, &declared, next_frontier)?;
            receipt.manifest_root = candidate_root;
            validate_receipt_for_storage(&receipt)?;
            Ok(super::PreparedMutation {
                new_records,
                sealed,
                transaction: Some(transaction),
                receipt,
                next_frontier,
                declared,
                candidate_root,
                manifest_space: ctx.space.clone(),
                manifest_authority_frontier: ctx.authority_frontier.clone(),
                manifest_signer: ctx.signer.signer_key(),
            })
        })();

        match assembled {
            Ok(data) => self
                .finalize_prepared_action(
                    ctx,
                    super::PreparedActionState::Mutation { fabric, data },
                )
                .map(ActionOutcome::Committed),
            Err(error) => {
                if lock_fabric(&self.fabric).rollback(fabric).is_err() {
                    self.poisoned = true;
                    Err(Failure::OutcomeUnknown)
                } else {
                    Err(error)
                }
            }
        }
    }
}

fn row_evidence(
    body: &PriorBodyEvidence,
    snapshot: &fabric::BodySnapshot,
) -> Result<[u8; 32], Failure> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/prior-semantic-row/1");
    let coordinates = postcard::to_stdvec(&(&body.key, &body.binding))
        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
    hash.update(&coordinates);
    let bytes = snapshot.canonical_export_shared();
    hash.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hash.update(&bytes);
    let mut refs = body.content_refs.clone();
    refs.sort_unstable();
    refs.dedup();
    for reference in refs {
        hash.update(&reference);
    }
    Ok(*hash.finalize().as_bytes())
}

/// Stream an indexed prior Replica into fresh current signed transactions,
/// verify exact semantic equivalence, and leave the source untouched.
#[allow(clippy::too_many_arguments)]
pub fn migrate_prior<F>(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
    context: &CommitContext<'_>,
    keys: Arc<dyn BodyKeySource>,
    authority: &dyn AuthoritySource,
    actor: &mechanics::ids::ActorId,
    device: &mechanics::ids::DeviceId,
    demand: impl Fn(&crate::body::WorldId) -> Result<Vec<u8>, Failure>,
    authorize: F,
) -> Result<Verification, Failure>
where
    F: Fn(&crate::body::WorldId, &Core) -> Result<Vec<u8>, mechanics::authorization::Refusal>,
{
    let source = PriorReplicaSource::open(source, keys.clone())?;
    if source.space() != Some(context.space) {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    if !authority.signer_authorized(
        &source.manifest().signer,
        &source.manifest().authority_frontier,
    ) {
        return Err(Failure::Unauthorized(
            mechanics::authorization::Refusal::Denied(
                mechanics::authorization::DenialReason::Internal(
                    "prior manifest signer has no standing at its frontier",
                ),
            ),
        ));
    }
    let source_manifest = *blake3::hash(&source.manifest().bytes).as_bytes();
    let target_path = target.as_ref().to_path_buf();
    let mut target = Replica::open(&target_path, keys.clone())?;
    if target
        .space
        .as_ref()
        .is_some_and(|space| space != context.space)
    {
        return Err(Failure::Integrity(Defect::Encoding));
    }

    let mut after = None;
    loop {
        let page = source.content_page(after, 4096)?;
        let missing = page
            .descriptors
            .iter()
            .filter(|descriptor| {
                target
                    .content_descriptor(&descriptor.content_ref())
                    .is_none()
            })
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            target.commit_content(context, &missing)?;
        }
        let Some(next) = page.next else { break };
        after = Some(next);
    }

    let mut expected_rows = Vec::new();
    let mut after = None;
    loop {
        let page = source.body_page(after, 4096)?;
        let mut by_world =
            BTreeMap::<crate::body::WorldId, Vec<(PriorBodyEvidence, fabric::BodySnapshot)>>::new();
        for body in page.bodies {
            for head in &body.heads {
                if !authority.signer_authorized(
                    &head.transaction.signer,
                    &head.transaction.authority_frontier,
                ) {
                    return Err(Failure::Unauthorized(
                        mechanics::authorization::Refusal::Denied(
                            mechanics::authorization::DenialReason::Internal(
                                "prior transaction signer has no standing at its frontier",
                            ),
                        ),
                    ));
                }
            }
            let snapshot = prior_body_snapshot(&body)?;
            expected_rows.push(row_evidence(&body, &snapshot)?);
            by_world
                .entry(body.key.world.clone())
                .or_default()
                .push((body, snapshot));
        }
        for (world, mut rows) in by_world {
            rows.sort_by(|left, right| left.0.key.cmp(&right.0.key));
            for batch in rows.chunks(SEMANTIC_MIGRATION_BATCH) {
                let digest = migration_digest(&source_manifest, batch)?;
                let mut request = [0u8; 16];
                request.copy_from_slice(&digest[..16]);
                let effect = PriorMigrationEffect {
                    version: 1,
                    source_manifest,
                    world: world.clone(),
                    bodies: batch.iter().map(|(body, _)| body.key.clone()).collect(),
                };
                let effect = postcard::to_stdvec(&effect)
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                let authorizer = MigrationAuthorizer {
                    world: &world,
                    authorize: &authorize,
                };
                let authorization = CommitAuthorization {
                    actor: actor.as_str(),
                    parent_manifest_root: target.current_manifest_root(),
                    demand: demand(&world)?,
                    intent_digest: digest,
                    authorizer: &authorizer,
                };
                target.commit_prior_batch(
                    context,
                    &authorization,
                    &world,
                    device,
                    &request,
                    &digest,
                    effect,
                    batch,
                )?;
            }
        }
        let Some(next) = page.next else { break };
        after = Some(next);
    }

    let receipt_count = source.receipt_count();
    let mut after = None;
    let mut verified_receipts = 0u64;
    loop {
        let page = source.receipt_page(after, 4096)?;
        verified_receipts = verified_receipts
            .checked_add(u64::try_from(page.receipts.len()).unwrap_or(u64::MAX))
            .ok_or(Failure::Integrity(Defect::Encoding))?;
        let Some(next) = page.next else { break };
        after = Some(next);
    }
    if verified_receipts != receipt_count {
        return Err(Failure::Integrity(Defect::Index));
    }

    drop(target);
    let rebuilt = Replica::open(target_path, keys)?;
    let snapshot = rebuilt.read_snapshot();
    if rebuilt.body_count() != source.body_count() {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    let mut actual_rows = Vec::new();
    for key in snapshot.body_keys() {
        let body = snapshot
            .body_ix(&key)
            .ok_or(Failure::Integrity(Defect::Index))?;
        let image = snapshot
            .resolve_body_image(body)
            .map_err(|_| Failure::Integrity(Defect::MissingMaterial))?;
        let prior = PriorBodyEvidence {
            key: key.clone(),
            binding: snapshot
                .binding(&key)
                .cloned()
                .ok_or(Failure::Integrity(Defect::Index))?,
            chain: ReplicaFrontier::EMPTY,
            interpreted: true,
            heads: Vec::new(),
            content_refs: rebuilt
                .declared_content(&key)
                .into_iter()
                .map(|reference| *reference.as_bytes())
                .collect(),
        };
        actual_rows.push(row_evidence(&prior, &image)?);
    }
    expected_rows.sort_unstable();
    actual_rows.sort_unstable();
    if expected_rows != actual_rows {
        return Err(Failure::Integrity(Defect::CorruptMaterial));
    }
    let mut evidence = blake3::Hasher::new();
    evidence.update(b"lait/prior-semantic-equivalence/1");
    evidence.update(&source_manifest);
    for row in &actual_rows {
        evidence.update(row);
    }
    Ok(Verification {
        evidence: *evidence.finalize().as_bytes(),
        bodies: source.body_count(),
        receipts: verified_receipts,
    })
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
    let source_path = source.as_ref();
    if let Ok(indexed) = PriorReplicaSource::open(source_path, keys.clone()) {
        if indexed.body_count() != 0 {
            return Err(Failure::NeedsSemanticMigration {
                bodies: indexed.body_count(),
            });
        }
    }
    let source = journal::GenerationSource::open(source_path).map_err(map_journal)?;
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

    // A whole-Body signed head cannot be rewritten into an ArtifactRef closure
    // without its author. Pre-v1 migrations replay non-empty Bodies through a
    // current Replica instead of manufacturing a signature here.
    if !prior.bodies.is_empty() {
        return Err(Failure::NeedsSemanticMigration {
            bodies: u64::try_from(prior.bodies.len()).unwrap_or(u64::MAX),
        });
    }
    let bodies: BTreeMap<BodyKey, BodyRecord> = BTreeMap::new();
    let mut added = Vec::new();
    let mut required = BTreeSet::new();

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

    let mut receipt_owners = BTreeMap::<[u8; 32], (Object, u64)>::new();
    for (object, _) in receipts.values() {
        match receipt_owners.get_mut(&object.hash) {
            Some((held, owners)) if held == object => {
                *owners = owners
                    .checked_add(1)
                    .ok_or(Failure::Integrity(Defect::Index))?;
            }
            Some(_) => return Err(Failure::Integrity(Defect::CorruptMaterial)),
            None => {
                receipt_owners.insert(object.hash, (*object, 1));
            }
        }
    }
    let ownership_entries = receipt_owners
        .iter()
        .map(|(hash, (object, owners))| {
            Ok(crate::index::IndexEntry {
                key: ownership_index_key(hash),
                value: postcard::to_stdvec(&IndexedOwnership {
                    object: *object,
                    class: OwnedObjectClass::DeferredReceipt,
                    owners: *owners,
                })
                .map_err(|_| Failure::Integrity(Defect::Encoding))?,
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    let ownership_index_root = crate::index::build_index(ownership_entries, &mut sink)
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
        receipt_count: u64::try_from(receipts.len()).unwrap_or(u64::MAX),
        receipt_material_bytes: receipts.values().fold(0u64, |total, (_, bytes)| {
            total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        }),
        generation_index_root: None,
        ownership_index_root,
        manifest_root: Some(root_object),
    };
    let meta = postcard::to_stdvec(&meta).map_err(|_| Failure::Integrity(Defect::Encoding))?;
    let roots: Vec<([u8; 32], u64)> = body_index_root
        .into_iter()
        .chain(manifest_body_root)
        .chain(receipt_index_root)
        .map(|root| (root.hash, root.count))
        .collect();
    let lazy_roots: Vec<([u8; 32], u64)> = ownership_index_root
        .into_iter()
        .map(|root| (root.hash, root.count))
        .collect();
    let caller_index = journal::Index {
        roots: &roots,
        lazy_roots: &lazy_roots,
        nodes: &sink.written,
    };
    let mut target_store = journal::Store::open(&target_path).map_err(map_journal)?;
    if target_store.manifest().is_some() {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    let receipt_hashes: BTreeSet<[u8; 32]> = receipt_owners.keys().copied().collect();
    let (deferred_added, eager_added): (Vec<Vec<u8>>, Vec<Vec<u8>>) = added
        .into_iter()
        .partition(|bytes| receipt_hashes.contains(&object_ref(bytes).hash));
    target_store
        .commit_classified(
            &eager_added,
            &[],
            journal::Deferred {
                added: &deferred_added,
                removed: &[],
            },
            caller_index,
            meta,
        )
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
    use crate::transaction::{SeedSigner, Signer, StaticAuthorizer};
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

    const PRIOR_EPOCH: [u8; 16] = [0x31; 16];
    const PRIOR_KEY: [u8; 32] = [0x42; 32];

    struct PriorKeys;

    impl BodyKeySource for PriorKeys {
        fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
            Some(AuthorizedBodyKey::for_authorized_epoch(
                PRIOR_EPOCH,
                PRIOR_KEY,
            ))
        }

        fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
            (epoch == &PRIOR_EPOCH)
                .then(|| AuthorizedBodyKey::for_authorized_epoch(PRIOR_EPOCH, PRIOR_KEY))
        }
    }

    struct AnyStanding;

    impl AuthoritySource for AnyStanding {
        fn signer_authorized(&self, _signer: &[u8; 32], _frontier: &AuthorityFrontier) -> bool {
            true
        }
    }

    #[derive(Serialize)]
    struct PriorIndexedManifest {
        format_version: u8,
        sequence: u64,
        required_object_index_root: Option<([u8; 32], u64)>,
        caller_meta: Option<Object>,
        caller_index_roots: Vec<([u8; 32], u64)>,
    }

    fn write_object(root: &Path, bytes: &[u8]) -> Object {
        let object = object_ref(bytes);
        let name = object
            .hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        std::fs::write(root.join("objects").join(name), bytes).unwrap();
        object
    }

    fn write_index(root: &Path, entries: Vec<crate::index::IndexEntry>) -> crate::index::ChildRef {
        let mut sink = crate::index::NodeSink::default();
        let child = crate::index::build_index(entries, &mut sink)
            .unwrap()
            .expect("non-empty fixture index");
        for bytes in sink.written {
            write_object(root, &bytes);
        }
        child
    }

    fn legacy_transaction(
        space: &SpaceId,
        descriptor: LegacyDescriptor,
        seed: &[u8; 32],
    ) -> LegacyTransaction {
        use mechanics::authorization::{
            policy_evidence_digest, AuthorizationDemand, AuthorizationReceipt, PolicyCapability,
            Resource,
        };
        let signer = SeedSigner(seed);
        let demand = AuthorizationDemand::require(
            PolicyCapability::new("com.example.prior", "write"),
            Resource::root("com.example.prior"),
        );
        let demand_bytes = demand.encode_canonical().unwrap();
        let mut space_bytes = [0u8; 29];
        space_bytes.copy_from_slice(space.as_str().as_bytes());
        let core = LegacyCore {
            version: 1,
            space: space_bytes,
            parent_manifest_root: [0u8; 32],
            replica_frontier: ReplicaFrontier::new([0x61; 32], 1),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(Vec::new()),
            actor: "act_0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            signer: signer.signer_key(),
            intent_digest: [0x62; 32],
            operations_digest: [0x63; 32],
            demand: demand_bytes,
            descriptors: vec![descriptor],
        };
        let receipt = AuthorizationReceipt {
            space: space.as_str().to_string(),
            world: "com.example.prior".to_string(),
            actor: core.actor.clone(),
            device: core.signer,
            authority_frontier: core.authority_frontier.as_bytes().to_vec(),
            authority_checkpoint_commitment: [0u8; 32],
            policy_evidence_digest: policy_evidence_digest(&[]),
            parent_manifest_root: core.parent_manifest_root,
            implementation_id: [0u8; 32],
            intent_digest: core.intent_digest,
            demand_digest: demand.digest().unwrap(),
            effect_operations_digest: core.operations_digest,
            body_transaction_core_digest: legacy_core_digest(&core).unwrap(),
            decision: 1,
        }
        .encode();
        let preimage_body = postcard::to_stdvec(&(&core, &receipt)).unwrap();
        let preimage = legacy_length_framed(LEGACY_TRANSACTION_DOMAIN, &preimage_body).unwrap();
        LegacyTransaction {
            core,
            authorization_receipt: receipt,
            signature_algorithm: 1,
            signature: signer.sign_preimage(&preimage),
        }
    }

    fn indexed_prior_fixture(name: &str) -> (std::path::PathBuf, BodyKey, Vec<u8>) {
        let root = directory(name);
        let space = SpaceId::from_digest([0x51; 16]);
        let world = crate::body::WorldId::parse("com.example.prior").unwrap();
        let key = BodyKey::new(world, crate::body::BodyId::from_bytes([0x52; 16]));
        let binding = super::super::BodyBinding {
            schema: crate::body::SchemaId::parse("fact").unwrap(),
            schema_version: 1,
            encoding: crate::body::EncodingId::parse("bytes").unwrap(),
            mutation_model: super::super::MUTATION_ATOMIC,
        };
        let chain = ReplicaFrontier::new([0x53; 32], 1);
        let value = b"prior signed fact".to_vec();
        let material = LegacyMaterial {
            version: 1,
            mutation_model: super::super::MUTATION_ATOMIC,
            payload: fabric::BodyExport::Atomic(value.clone()),
            base_frontier: ReplicaFrontier::EMPTY,
            resulting_frontier: chain,
        };
        let opening = AuthorizedBodyKey::for_authorized_epoch(PRIOR_EPOCH, PRIOR_KEY);
        let envelope =
            mechanics::authorization::body_seal(&opening, &postcard::to_stdvec(&material).unwrap())
                .unwrap();
        let protected = write_object(&root, &envelope);
        let descriptor = LegacyDescriptor {
            world: key.world.clone(),
            body: key.body.clone(),
            schema: binding.schema.clone(),
            schema_version: binding.schema_version,
            encoding: binding.encoding.clone(),
            content_commitment: crate::body::ContentCommitment::over_protected_payload(&envelope)
                .as_bytes(),
        };
        let tx = legacy_transaction(&space, descriptor.clone(), &[0x54; 32]);
        let tx_bytes = postcard::to_stdvec(&tx).unwrap();
        let transaction = write_object(&root, &tx_bytes);
        let head = PriorBodyHead {
            tx: legacy_transaction_id(&tx_bytes),
            descriptor_hash: *blake3::hash(&postcard::to_stdvec(&descriptor).unwrap()).as_bytes(),
            tx_commitment: *blake3::hash(&tx_bytes).as_bytes(),
            protected: Some(protected),
            transaction: Some(transaction),
            protected_len: protected.len,
            tx_len: transaction.len,
        };
        let record = PriorBodyRecord {
            binding,
            chain,
            heads: vec![head.clone()],
            interpreted: true,
        };
        let body_value = postcard::to_stdvec(&LegacyIndexedBody {
            key: key.clone(),
            record,
        })
        .unwrap();
        let body_root = write_index(
            &root,
            vec![crate::index::IndexEntry {
                key: body_index_key(&key),
                value: body_value,
            }],
        );
        // Empty content still has one canonical leaf. Keeping a zero-leaf
        // descriptor here used to make this prior-store fixture bypass the
        // current content geometry instead of exercising the migration
        // verifier against evidence a real v2 writer could have produced.
        let empty_leaf = crate::content::ChunkLeaf {
            chunk_index: 0,
            ciphertext_len: 0,
            ciphertext_hash: *blake3::hash(&[]).as_bytes(),
        };
        let content_descriptor = crate::content::ContentDescriptor {
            format_version: crate::content::CONTENT_FORMAT_VERSION,
            space: space.as_str().to_string(),
            content_nonce: [0x56; 16],
            plaintext_len: 0,
            chunk_plaintext_len: crate::content::CHUNK_PLAINTEXT_LEN,
            chunk_count: 1,
            ciphertext_merkle_root: crate::content::merkle_root(&[empty_leaf]),
            epoch: PRIOR_EPOCH,
        };
        content_descriptor.validate().unwrap();
        let content_id = *content_descriptor.content_ref().as_bytes();
        let content_root = write_index(
            &root,
            vec![crate::index::IndexEntry {
                key: crate::manifest::content_index_key(&content_id),
                value: content_descriptor.encode(),
            }],
        );
        let advertised = crate::manifest::ManifestEntry::declaring(
            key.clone(),
            vec![crate::manifest::ManifestHead {
                descriptor_hash: head.descriptor_hash,
                transaction_commitment: head.tx_commitment,
            }],
            vec![content_id],
        )
        .unwrap();
        let advertised_root = write_index(
            &root,
            vec![crate::index::IndexEntry {
                key: body_index_key(&key),
                value: advertised.encode(),
            }],
        );
        let signer = SeedSigner(&[0x55; 32]);
        let signed_root = crate::manifest::ManifestRoot::sign_with(
            &space,
            tx.core.replica_frontier,
            Some(advertised_root),
            Some(content_root),
            AuthorityFrontier::from_canonical_bytes(Vec::new()),
            &signer,
        )
        .unwrap();
        let signed_root_object = write_object(&root, &signed_root.encode());
        let meta = PriorIndexedStoreMeta {
            format_version: 2,
            space: Some(space),
            frontier: tx.core.replica_frontier,
            quota: QuotaConfig::default(),
            body_index_root: Some(body_root),
            manifest_body_root: Some(advertised_root),
            content_index_root: Some(content_root),
            receipt_index_root: None,
            manifest_root: Some(signed_root_object),
        };
        let meta_bytes = postcard::to_stdvec(&meta).unwrap();
        let meta_object = write_object(&root, &meta_bytes);

        let mut required = vec![protected, transaction, signed_root_object, meta_object];
        required.sort_by_key(|object| object.hash);
        required.dedup_by_key(|object| object.hash);
        let required_root = write_index(
            &root,
            required
                .into_iter()
                .map(|object| crate::index::IndexEntry {
                    key: object.hash,
                    value: object.len.to_be_bytes().to_vec(),
                })
                .collect(),
        );
        let manifest = PriorIndexedManifest {
            format_version: 2,
            sequence: 7,
            required_object_index_root: Some((required_root.hash, required_root.count)),
            caller_meta: Some(meta_object),
            caller_index_roots: vec![
                (body_root.hash, body_root.count),
                (advertised_root.hash, advertised_root.count),
                (content_root.hash, content_root.count),
            ],
        };
        std::fs::write(
            root.join("current-manifest"),
            postcard::to_stdvec(&manifest).unwrap(),
        )
        .unwrap();
        (root, key, value)
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

    #[test]
    fn indexed_prior_source_pages_verified_signed_evidence_without_rewriting_it() {
        let (source, key, value) = indexed_prior_fixture("indexed-source");
        let prior = PriorReplicaSource::open(&source, Arc::new(PriorKeys)).unwrap();
        assert_eq!(prior.body_count(), 1);
        assert_eq!(prior.manifest().body_index_root.unwrap().1, 1);
        let page = prior.body_page(None, 1).unwrap();
        assert!(page.next.is_none());
        assert_eq!(page.bodies.len(), 1);
        assert_eq!(page.bodies[0].key, key);
        assert_eq!(page.bodies[0].heads.len(), 1);
        assert_eq!(
            page.bodies[0].heads[0].material.as_ref().unwrap().payload,
            fabric::BodyExport::Atomic(value)
        );
        assert_eq!(page.bodies[0].content_refs.len(), 1);
        let content = prior.content_page(None, 1).unwrap();
        assert!(content.next.is_none());
        assert_eq!(content.descriptors.len(), 1);
        assert_eq!(
            content.descriptors[0].content_ref().as_bytes(),
            &page.bodies[0].content_refs[0]
        );

        let space = prior.space().unwrap().clone();
        let seed = [0x77; 32];
        let signer = SeedSigner(&seed);
        let context = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: AuthorityFrontier::from_canonical_bytes(Vec::new()),
        };
        let target = directory("semantic-target");
        assert_eq!(
            build_prior(&source, &target, &context, Arc::new(PriorKeys)).unwrap_err(),
            Failure::NeedsSemanticMigration { bodies: 1 }
        );
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(target);
    }

    #[test]
    fn nonempty_prior_facts_cross_only_as_fresh_current_transactions() {
        use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};

        let (source, key, value) = indexed_prior_fixture("semantic-source");
        let target = directory("semantic-current");
        let source_manifest = std::fs::read(source.join("current-manifest")).unwrap();
        let space = SpaceId::from_digest([0x51; 16]);
        let seed = [0x77; 32];
        let signer = SeedSigner(&seed);
        let context = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: AuthorityFrontier::from_canonical_bytes(Vec::new()),
        };
        let actor = mechanics::ids::ActorId::from_incept_hash(
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        let device = mechanics::actor::device_from_seed(&seed);
        let run = || {
            migrate_prior(
                &source,
                &target,
                &context,
                Arc::new(PriorKeys),
                &AnyStanding,
                &actor,
                &device,
                |world| {
                    AuthorizationDemand::require(
                        PolicyCapability::new(world.as_str(), "space.admin"),
                        Resource::root(world.as_str()),
                    )
                    .encode_canonical()
                    .map_err(|_| Failure::Integrity(Defect::Encoding))
                },
                |world, core| {
                    StaticAuthorizer {
                        world: world.clone(),
                        implementation_id: [0; 32],
                    }
                    .authorize(core)
                },
            )
        };
        let first = run().unwrap();
        assert_eq!(first.bodies(), 1);
        assert_eq!(first.receipts(), 0);
        assert_eq!(
            std::fs::read(source.join("current-manifest")).unwrap(),
            source_manifest
        );

        let rebuilt = Replica::open(&target, Arc::new(PriorKeys)).unwrap();
        assert_eq!(rebuilt.body_count(), 1);
        let snapshot = rebuilt.read_snapshot();
        assert_eq!(snapshot.read(&key).unwrap(), value);
        assert_eq!(rebuilt.declared_content(&key).len(), 1);

        let replay = run().unwrap();
        assert_eq!(replay.evidence(), first.evidence());
        assert_eq!(
            Replica::open(&target, Arc::new(PriorKeys))
                .unwrap()
                .body_count(),
            1
        );

        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(target);
    }

    #[test]
    fn prior_transaction_and_material_tampering_are_typed_integrity_failures() {
        let space = SpaceId::from_digest([0x71; 16]);
        let key = BodyKey::new(
            crate::body::WorldId::parse("com.example.prior").unwrap(),
            crate::body::BodyId::from_bytes([0x72; 16]),
        );
        let descriptor = LegacyDescriptor {
            world: key.world,
            body: key.body,
            schema: crate::body::SchemaId::parse("fact").unwrap(),
            schema_version: 1,
            encoding: crate::body::EncodingId::parse("bytes").unwrap(),
            content_commitment: [0x73; 32],
        };
        let mut tx = legacy_transaction(&space, descriptor.clone(), &[0x74; 32]);
        assert!(decode_legacy_transaction(&postcard::to_stdvec(&tx).unwrap()).is_ok());
        tx.signature[0] ^= 0x80;
        assert!(matches!(
            decode_legacy_transaction(&postcard::to_stdvec(&tx).unwrap()),
            Err(Failure::Integrity(_))
        ));

        let mut tx = legacy_transaction(&space, descriptor.clone(), &[0x74; 32]);
        tx.authorization_receipt[0] ^= 0x80;
        assert!(matches!(
            decode_legacy_transaction(&postcard::to_stdvec(&tx).unwrap()),
            Err(Failure::Integrity(_))
        ));

        let mut tx = legacy_transaction(&space, descriptor.clone(), &[0x74; 32]);
        tx.core.descriptors.push(descriptor);
        assert!(matches!(
            decode_legacy_transaction(&postcard::to_stdvec(&tx).unwrap()),
            Err(Failure::Integrity(_))
        ));

        let mut tx = legacy_transaction(&space, tx.core.descriptors[0].clone(), &[0x74; 32]);
        let mut lower = tx.core.descriptors[0].clone();
        lower.body = crate::body::BodyId::from_bytes([0x01; 16]);
        tx.core.descriptors.push(lower);
        assert!(matches!(
            decode_legacy_transaction(&postcard::to_stdvec(&tx).unwrap()),
            Err(Failure::Integrity(_))
        ));

        let record = PriorBodyRecord {
            binding: super::super::BodyBinding {
                schema: crate::body::SchemaId::parse("fact").unwrap(),
                schema_version: 1,
                encoding: crate::body::EncodingId::parse("bytes").unwrap(),
                mutation_model: super::super::MUTATION_ATOMIC,
            },
            chain: ReplicaFrontier::new([0x75; 32], 1),
            heads: Vec::new(),
            interpreted: true,
        };
        let material = LegacyMaterial {
            version: 1,
            mutation_model: super::super::MUTATION_ATOMIC,
            payload: fabric::BodyExport::Atomic(b"fact".to_vec()),
            base_frontier: ReplicaFrontier::EMPTY,
            resulting_frontier: record.chain,
        };
        let key = AuthorizedBodyKey::for_authorized_epoch(PRIOR_EPOCH, PRIOR_KEY);
        let mut envelope =
            mechanics::authorization::body_seal(&key, &postcard::to_stdvec(&material).unwrap())
                .unwrap();
        assert!(open_legacy_material(&key, &envelope, &record).is_ok());
        let last = envelope.len() - 1;
        envelope[last] ^= 0x80;
        assert!(matches!(
            open_legacy_material(&key, &envelope, &record),
            Err(Failure::Integrity(_))
        ));
        let wrong = AuthorizedBodyKey::for_authorized_epoch([0x76; 16], PRIOR_KEY);
        assert_eq!(
            open_legacy_material(&wrong, &envelope, &record).unwrap_err(),
            Failure::BodyKeyUnavailable
        );
    }

    #[test]
    fn prior_advertised_heads_refuse_duplicate_or_noncanonical_record_order() {
        let key = BodyKey::new(
            crate::body::WorldId::parse("com.example.prior").unwrap(),
            crate::body::BodyId::from_bytes([0x78; 16]),
        );
        let binding = super::super::BodyBinding {
            schema: crate::body::SchemaId::parse("fact").unwrap(),
            schema_version: 1,
            encoding: crate::body::EncodingId::parse("bytes").unwrap(),
            mutation_model: super::super::MUTATION_ATOMIC,
        };
        let head = |byte| PriorBodyHead {
            tx: [byte; 32],
            descriptor_hash: [byte; 32],
            tx_commitment: [byte; 32],
            protected: None,
            transaction: None,
            protected_len: 0,
            tx_len: 0,
        };
        let advertised = crate::manifest::ManifestEntry::declaring(
            key.clone(),
            vec![
                crate::manifest::ManifestHead {
                    descriptor_hash: [1; 32],
                    transaction_commitment: [1; 32],
                },
                crate::manifest::ManifestHead {
                    descriptor_hash: [2; 32],
                    transaction_commitment: [2; 32],
                },
            ],
            Vec::new(),
        )
        .unwrap();
        let record = |heads| PriorBodyRecord {
            binding: binding.clone(),
            chain: ReplicaFrontier::new([1; 32], 1),
            heads,
            interpreted: false,
        };
        assert!(
            validate_prior_advertisement(&key, &record(vec![head(1), head(2)]), &advertised)
                .is_ok()
        );
        assert!(
            validate_prior_advertisement(&key, &record(vec![head(2), head(1)]), &advertised)
                .is_err()
        );
        assert!(
            validate_prior_advertisement(&key, &record(vec![head(1), head(1)]), &advertised)
                .is_err()
        );
    }

    #[test]
    fn interpreted_prior_body_never_collapses_a_missing_epoch_into_opaque() {
        let (source, _, _) = indexed_prior_fixture("missing-key");
        let prior = PriorReplicaSource::open(&source, Arc::new(NoKeys)).unwrap();
        assert_eq!(
            prior.body_page(None, 1).unwrap_err(),
            Failure::BodyKeyUnavailable
        );
        let _ = std::fs::remove_dir_all(source);
    }
}
