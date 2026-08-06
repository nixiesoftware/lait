//! `Transaction` — the signed Body-transaction envelope, its core, and
//! its descriptors (`lait/body-transaction/2`), the protection boundary.
//!
//! A transaction is a device-signed **envelope** over two parts:
//!
//! - the [`Core`]: Space, parent Manifest root, resulting
//!   Replica frontier, referenced authority frontier, acting principal,
//!   canonical authorization demand, intent/operations digests, and the
//!   ordered BodyKey-sorted set of public [`Descriptor`]s with their
//!   ciphertext commitments;
//! - the mechanics-derived **authorization receipt**
//!   ([`mechanics::authorization::AuthorizationReceipt`], canonical bytes), which
//!   binds the core digest, the demand, the evidence, the checkpoint
//!   commitment, and the pinned coordinates.
//!
//! The digest cycle is broken exactly as specified: the **core digest**
//! hashes the complete canonical core (excluding the receipt, the outer
//! signature, and the outer id); Mechanics binds that digest inside the
//! receipt; the device then signs the envelope `{ core, receipt }`; and the
//! **full signed-envelope digest is the transaction id** referenced by
//! Manifest entries and request receipts.
//!
//! There is no plaintext hash anywhere: the commitment is ciphertext-only
//! ([`crate::body::ContentCommitment`]), which avoids an equality oracle.
//!
//! **Two levels of verification, deliberately separate.**
//! [`Transaction::verify`] is the *opaque structural* check any Station
//! can run without membership state: canonical shape, descriptor ordering,
//! demand canonicality, the receipt's byte-exact binding to the core, and the
//! committing signature. It is **not** an authority check. Before a
//! transaction is retained or incorporated, mechanics must also prove the
//! receipt against the referenced historical frontier — **no World callback
//! runs**; [`Transaction::verify_authorized`] runs the structural check
//! and then consults the mechanics-provided [`AuthoritySource`].

pub use crate::replica::{
    ActionOutcome, CommitAuthorization, CommitContext, StaticAuthorizer, TransactionAuthorizer,
};

/// Typed failures produced while committing or incorporating a transaction.
pub mod commit {
    pub use crate::replica::{Defect, Failure, Invalid};
}

pub use crate::algebra::MAX_OPS_PER_TRANSACTION;

use mechanics::authorization::AuthorizationReceipt;
use mechanics::ids::SpaceId;
use serde::{Deserialize, Serialize};

use crate::body::ContentCommitment;
use crate::frontier::{AuthorityFrontier, ReplicaFrontier};
use crate::ids::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};

/// The signing domain for a Body transaction envelope.
pub const BODY_TRANSACTION_DOMAIN: &[u8] = b"lait/body-transaction/2";
/// BLAKE3 derive-key context for the core digest.
const CORE_DIGEST_CONTEXT: &str = "lait.body-transaction-core.v1";
/// BLAKE3 derive-key context for the transaction id (full envelope digest).
const TRANSACTION_ID_CONTEXT: &str = "lait.body-transaction-id.v1";
/// Ed25519 algorithm tag.
pub const SIG_ALG_ED25519: u8 = 1;
/// Maximum descriptors in one transaction.
pub const MAX_DESCRIPTORS: usize = 4096;
/// Maximum encoded transaction size (1 MiB).
pub const MAX_TRANSACTION: usize = 1024 * 1024;
/// The fixed rendered-SpaceId length.
pub const SPACE_ID_LEN: usize = 29;
/// The "no parent" Manifest root (a fresh store's first commit).
pub const NO_PARENT_ROOT: [u8; 32] = [0u8; 32];

/// A public Body descriptor in canonical wire form. Binding to the enclosing
/// transaction is positional: descriptors live inside the signed core, so a
/// descriptor can never be transplanted into another transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    pub world: WorldId,
    pub body: BodyId,
    pub schema: SchemaId,
    pub schema_version: u32,
    pub encoding: EncodingId,
    pub content_commitment: [u8; 32],
}

impl Descriptor {
    /// The BodyKey this descriptor addresses (the sort key).
    pub fn key(&self) -> BodyKey {
        BodyKey::new(self.world.clone(), self.body.clone())
    }

    /// Whether a protected payload's ciphertext matches this descriptor's
    /// commitment — the ciphertext-only content check an opaque retainer runs
    /// before any decryption is attempted.
    pub fn commits_to(&self, protected_payload: &[u8]) -> bool {
        ContentCommitment::over_protected_payload(protected_payload).as_bytes()
            == self.content_commitment
    }
}

/// The transaction core: everything the receipt binds, excluding the receipt
/// itself, the outer signature, and the outer transaction id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Core {
    pub version: u8,
    pub space: [u8; SPACE_ID_LEN],
    /// The committed Manifest root this transaction was authored against
    /// ([`NO_PARENT_ROOT`] for a fresh store's first commit). Local submit
    /// requires it to equal the current committed root; remote work may name
    /// a verified historical or concurrent parent.
    pub parent_manifest_root: [u8; 32],
    /// The resulting Replica frontier this transaction advances its author to.
    pub replica_frontier: ReplicaFrontier,
    /// The authority frontier authorization was evaluated at.
    pub authority_frontier: AuthorityFrontier,
    /// The acting principal (canonical ActorId text).
    pub actor: String,
    /// The signing device's raw key.
    pub signer: [u8; 32],
    /// Digest of the signed intent payload.
    pub intent_digest: [u8; 32],
    /// Digest of the complete canonical staged operation set.
    pub operations_digest: [u8; 32],
    /// The canonical authorization-demand bytes (mandatory, non-empty).
    pub demand: Vec<u8>,
    /// The ordered, BodyKey-sorted descriptors.
    pub descriptors: Vec<Descriptor>,
}

impl Core {
    /// The canonical core digest — the value the authorization receipt binds.
    pub fn digest(&self) -> [u8; 32] {
        #[allow(
            clippy::expect_used,
            reason = "derived serialization of this validated transaction core is infallible"
        )]
        let bytes = postcard::to_stdvec(self).expect("postcard transaction core");
        blake3::derive_key(CORE_DIGEST_CONTEXT, &bytes)
    }
}

/// The signed Body-transaction envelope: core plus authorization receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub core: Core,
    /// Canonical [`AuthorizationReceipt`] bytes (opaque at the frame layer;
    /// structurally bound to the core by [`Transaction::verify`]).
    pub authorization_receipt: Vec<u8>,
    pub signature_algorithm: u8,
    #[serde(with = "serde_byte_array")]
    pub signature: [u8; 64],
}

/// Why a Body transaction failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnsupportedVersion(u8),
    UnsupportedSignatureAlgorithm(u8),
    NonCanonical,
    BadSpaceId,
    /// Empty or over the descriptor-count/size bound.
    BadDescriptorCount,
    /// Descriptors were not strictly BodyKey-sorted (unsorted or duplicated).
    UnsortedOrDuplicate,
    /// The demand bytes are absent or non-canonical.
    BadDemand,
    /// The authorization receipt is undecodable or does not bind this exact
    /// core (actor, device, Space, frontier, parent root, digests).
    ReceiptUnbound(ReceiptField),
    BadSignature,
    /// Structurally valid and correctly signed, but mechanics refused the
    /// receipt against the referenced historical frontier — carrying which
    /// check refused it. Produced only by [`Transaction::verify_authorized`].
    AuthorityUnverified(String),
    /// The referenced parent Manifest is not locally resolvable; retry once
    /// the exact material arrives. Never fall back to current state.
    ParentManifestUnavailable,
}

/// Which authorization-receipt binding failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptField {
    Encoding,
    Space,
    Actor,
    Device,
    AuthorityFrontier,
    ParentManifest,
    Intent,
    Operations,
    Demand,
    Core,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// A device signing capability the committing layer supplies (runtime's
/// `LocalIdentity` implements it). Replica builds the canonical preimage and
/// hands it here — it never sees seed bytes.
pub trait Signer: Send + Sync {
    /// The raw ed25519 public key of the signing device.
    fn signer_key(&self) -> [u8; 32];
    /// Sign an already-built canonical preimage.
    fn sign_preimage(&self, preimage: &[u8]) -> [u8; 64];
}

/// A seed-backed signer for tests and seed-holding callers.
pub struct SeedSigner<'a>(pub &'a [u8; 32]);

impl Signer for SeedSigner<'_> {
    fn signer_key(&self) -> [u8; 32] {
        #[allow(
            clippy::expect_used,
            reason = "device_from_seed always constructs an Ed25519 device identifier"
        )]
        mechanics::actor::device_from_seed(self.0)
            .key_bytes()
            .expect("seed-derived device key")
    }
    fn sign_preimage(&self, preimage: &[u8]) -> [u8; 64] {
        mechanics::actor::sign_detached(self.0, preimage)
    }
}

/// The mechanics-provided view of Space authority, consulted before material
/// is retained or incorporated. Replica owns no authority state; it asks this
/// seam. Mechanics implements it over the durable authority ledger.
pub trait AuthoritySource {
    /// Whether the device key `signer` was an admitted member with authoring
    /// standing at `authority_frontier` — the Manifest-advertisement
    /// legitimacy check.
    fn signer_authorized(&self, signer: &[u8; 32], authority_frontier: &AuthorityFrontier) -> bool;

    /// Verify a transaction's authorization receipt against **historical**
    /// mechanics state at its referenced frontier: actor resolution, demand
    /// evaluation, evidence digest, checkpoint commitment, and implementation
    /// activation. No World callback runs. Missing history is a retryable
    /// refusal.
    ///
    /// The default checks only that the signer had authoring standing at the
    /// referenced frontier — the minimal legitimacy any Station can prove. A
    /// real mechanics implementation MUST override it to verify the full
    /// authorization receipt (the orbital composition does).
    fn verify_transaction(&self, tx: &Transaction) -> Result<(), Refusal> {
        if self.signer_authorized(&tx.core.signer, &tx.core.authority_frontier) {
            Ok(())
        } else {
            Err(Refusal::Unauthorized(
                "the signer is not authorized at the referenced frontier".to_string(),
            ))
        }
    }
}

/// Why historical transaction standing was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The authority could not verify this transaction's receipt, and why.
    ///
    /// The reason is carried because this seam is where it was being lost. A
    /// mechanics receipt check names which of fourteen fields failed to bind —
    /// checkpoint, implementation, actor, evidence, demand digest — and a
    /// single-variant enum on this side meant the caller could learn only that
    /// *something* had. Four layers of `map_err(|_| …)` then reduced it to one
    /// word at the far end, where somebody was reading it.
    Unauthorized(String),
}

fn length_framed(domain: &[u8], body: &[u8]) -> Vec<u8> {
    let capacity = 6usize
        .saturating_add(domain.len())
        .saturating_add(body.len());
    let mut out = Vec::with_capacity(capacity);
    let domain_len = u16::try_from(domain.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&domain_len.to_be_bytes());
    out.extend_from_slice(domain);
    let body_len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&body_len.to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn space_bytes(space: &SpaceId) -> Option<[u8; SPACE_ID_LEN]> {
    <[u8; SPACE_ID_LEN]>::try_from(space.as_str().as_bytes()).ok()
}

/// The inputs the committing layer supplies to sign a transaction.
pub struct SignRequest<'a> {
    pub space: &'a SpaceId,
    pub parent_manifest_root: [u8; 32],
    pub replica_frontier: ReplicaFrontier,
    pub authority_frontier: AuthorityFrontier,
    pub actor: &'a str,
    pub intent_digest: [u8; 32],
    pub operations_digest: [u8; 32],
    pub demand: Vec<u8>,
    pub descriptors: Vec<Descriptor>,
}

impl Transaction {
    fn preimage(core: &Core, receipt: &[u8]) -> Vec<u8> {
        #[allow(
            clippy::expect_used,
            reason = "derived serialization of this validated envelope preimage is infallible"
        )]
        let body = postcard::to_stdvec(&(core, receipt)).expect("postcard envelope preimage");
        length_framed(BODY_TRANSACTION_DOMAIN, &body)
    }

    /// Build the core, hand its digest to `authorize` for the receipt, then
    /// sign the envelope. `authorize` receives the exact core digest the
    /// receipt must bind.
    pub fn sign_with(
        request: SignRequest<'_>,
        signer: &dyn Signer,
        authorize: impl FnOnce(&Core) -> Result<Vec<u8>, mechanics::authorization::Refusal>,
    ) -> Result<Self, mechanics::authorization::Refusal> {
        let core = Core {
            version: 1,
            space: space_bytes(request.space).ok_or(mechanics::authorization::Refusal::Denied(
                mechanics::authorization::DenialReason::Internal("space id is not valid bytes"),
            ))?,
            parent_manifest_root: request.parent_manifest_root,
            replica_frontier: request.replica_frontier,
            authority_frontier: request.authority_frontier,
            actor: request.actor.to_string(),
            signer: signer.signer_key(),
            intent_digest: request.intent_digest,
            operations_digest: request.operations_digest,
            demand: request.demand,
            descriptors: request.descriptors,
        };
        let receipt = authorize(&core)?;
        let mut tx = Self {
            core,
            authorization_receipt: receipt,
            signature_algorithm: SIG_ALG_ED25519,
            signature: [0u8; 64],
        };
        tx.signature = signer.sign_preimage(&Self::preimage(&tx.core, &tx.authorization_receipt));
        Ok(tx)
    }

    pub fn encode(&self) -> Vec<u8> {
        #[allow(
            clippy::expect_used,
            reason = "derived serialization of this validated transaction is infallible"
        )]
        postcard::to_stdvec(self).expect("postcard body-transaction")
    }

    /// The transaction id: the full signed-envelope digest.
    pub fn id(&self) -> [u8; 32] {
        blake3::derive_key(TRANSACTION_ID_CONTEXT, &self.encode())
    }

    /// Decode canonical bytes: size-bounded, exact decode/re-encode equality.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_TRANSACTION {
            return Err(Error::NonCanonical);
        }
        let tx: Self = postcard::from_bytes(bytes).map_err(|_| Error::NonCanonical)?;
        if tx.encode() != bytes {
            return Err(Error::NonCanonical);
        }
        Ok(tx)
    }

    /// The decoded, binding-checked authorization receipt.
    pub fn receipt(&self) -> Result<AuthorizationReceipt, Error> {
        AuthorizationReceipt::decode(&self.authorization_receipt)
            .map_err(|_| Error::ReceiptUnbound(ReceiptField::Encoding))
    }

    /// **Structural** verification only: canonical shape, descriptor
    /// ordering, canonical demand, receipt-to-core binding, and the
    /// committing signature. This does **not** prove authority — use
    /// [`Self::verify_authorized`] before retaining or incorporating.
    pub fn verify(&self) -> Result<(), Error> {
        let core = &self.core;
        if core.version != 1 {
            return Err(Error::UnsupportedVersion(core.version));
        }
        if self.signature_algorithm != SIG_ALG_ED25519 {
            return Err(Error::UnsupportedSignatureAlgorithm(
                self.signature_algorithm,
            ));
        }
        let space = std::str::from_utf8(&core.space)
            .ok()
            .and_then(SpaceId::parse)
            .ok_or(Error::BadSpaceId)?;

        if core.descriptors.is_empty() || core.descriptors.len() > MAX_DESCRIPTORS {
            return Err(Error::BadDescriptorCount);
        }
        // Strictly BodyKey-sorted, no duplicates.
        for window in core.descriptors.windows(2) {
            let [left, right] = window else { continue };
            if left.key() >= right.key() {
                return Err(Error::UnsortedOrDuplicate);
            }
        }
        // The demand must be present and canonical.
        let demand = mechanics::authorization::AuthorizationDemand::decode_canonical(&core.demand)
            .map_err(|_| Error::BadDemand)?;

        // The receipt must bind this exact core.
        let receipt = self.receipt()?;
        if receipt.space != space.as_str() {
            return Err(Error::ReceiptUnbound(ReceiptField::Space));
        }
        if receipt.actor != core.actor {
            return Err(Error::ReceiptUnbound(ReceiptField::Actor));
        }
        if receipt.device != core.signer {
            return Err(Error::ReceiptUnbound(ReceiptField::Device));
        }
        if receipt.authority_frontier != core.authority_frontier.as_bytes() {
            return Err(Error::ReceiptUnbound(ReceiptField::AuthorityFrontier));
        }
        if receipt.parent_manifest_root != core.parent_manifest_root {
            return Err(Error::ReceiptUnbound(ReceiptField::ParentManifest));
        }
        if receipt.intent_digest != core.intent_digest {
            return Err(Error::ReceiptUnbound(ReceiptField::Intent));
        }
        if receipt.effect_operations_digest != core.operations_digest {
            return Err(Error::ReceiptUnbound(ReceiptField::Operations));
        }
        if receipt.demand_digest != demand.digest().map_err(|_| Error::BadDemand)? {
            return Err(Error::ReceiptUnbound(ReceiptField::Demand));
        }
        if receipt.body_transaction_core_digest != core.digest() {
            return Err(Error::ReceiptUnbound(ReceiptField::Core));
        }

        if !mechanics::actor::verify_detached(
            &core.signer,
            &Self::preimage(core, &self.authorization_receipt),
            &self.signature,
        ) {
            return Err(Error::BadSignature);
        }
        Ok(())
    }

    /// Full verification for retention/incorporation: the structural
    /// [`Self::verify`] **and** the mechanics historical-receipt check at the
    /// referenced authority frontier. No World callback runs.
    pub fn verify_authorized(&self, authority: &dyn AuthoritySource) -> Result<(), Error> {
        self.verify()?;
        authority
            .verify_transaction(self)
            .map_err(|Refusal::Unauthorized(reason)| Error::AuthorityUnverified(reason))
    }
}
