//! Authorization demands, receipts, evidence, and refusal facts.

pub use crate::crypto::{
    body_epoch_id, body_open, content_chunk_open, open_sealed, AuthorizedBodyKey,
    ContentChunkBinding, SpaceKey, BODY_ENVELOPE_OVERHEAD, BODY_EPOCH_ID_LEN, KEY_LEN,
};
pub use crate::demand::{
    policy_evidence_digest, AuthorizationDemand, AuthorizationReceipt, Invalid, PolicyCapability,
    Resource, WorldAssignmentEvidence, MAX_CHILDREN, MAX_DEMAND_BYTES, MAX_DEMAND_DEPTH,
    MAX_NAME_BYTES, MAX_REQUIRE_LEAVES, MAX_RESOURCE_BYTES, MAX_RESOURCE_SEGMENTS,
    MAX_SEGMENT_BYTES,
};
pub use crate::ledger::{AuthorizationRequest, ReceiptExpectations, Refusal, SealedKeyRecord};

pub mod receipt {
    pub use crate::ledger::{Invalid, ReceiptField};
}

/// Why an accepted protection operation could not produce ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Randomness,
    Encryption,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Failure {}

fn map_failure(failure: crate::crypto::Failure) -> Failure {
    match failure {
        crate::crypto::Failure::Randomness => Failure::Randomness,
        crate::crypto::Failure::Encryption => Failure::Encryption,
    }
}

pub fn random_key() -> Result<SpaceKey, Failure> {
    crate::crypto::random_key().map_err(map_failure)
}

pub fn seal_to(
    recipient: &crate::ids::DeviceId,
    message: &[u8],
) -> Result<Option<Vec<u8>>, Failure> {
    crate::crypto::seal_to(recipient, message).map_err(map_failure)
}

pub fn body_seal(key: &AuthorizedBodyKey, plaintext: &[u8]) -> Result<Vec<u8>, Failure> {
    crate::crypto::body_seal(key, plaintext).map_err(map_failure)
}

pub fn content_chunk_seal(
    key: &AuthorizedBodyKey,
    binding: &ContentChunkBinding<'_>,
    plaintext: &[u8],
) -> Result<Vec<u8>, Failure> {
    crate::crypto::content_chunk_seal(key, binding, plaintext).map_err(map_failure)
}
