//! Authorization demands, receipts, evidence, and refusal facts.

pub use crate::crypto::{
    body_epoch_id, body_open, content_chunk_open, open_as_device, open_sealed, open_sealed_bound,
    AuthorizedBodyKey, ContentChunkBinding, DeviceSealed, SpaceKey, BODY_ENVELOPE_OVERHEAD,
    BODY_EPOCH_ID_LEN, KEY_LEN,
};
/// Multi-path custody for a secret that is not an authority share.
///
/// Only the general half of [`crate::custody`] crosses this seam. [`Package`]
/// and its FROST payload stay inside, because they bind themselves to a space,
/// an authority and a holder — comparisons that mean nothing to a caller with
/// none of those, and which an outside caller supplying its own values could
/// only perform against itself.
///
/// What a process-level adapter legitimately needs is the module's other
/// lesson: one data-encryption key under several independent unlock paths, so
/// the operating-system profile is never the durability boundary. The display
/// coordinator's identifier key is the first such holder outside this crate.
///
/// [`Package`]: crate::custody::Package
pub mod custody {
    pub use crate::custody::{Argon2Params, Custodied, SlotSpec, UnlockKey, CUSTODIED_VERSION};
}

pub use crate::demand::{
    policy_evidence_digest, AuthorizationDemand, AuthorizationReceipt, Invalid, PolicyCapability,
    Resource, WorldAssignmentEvidence, MAX_CHILDREN, MAX_DEMAND_BYTES, MAX_DEMAND_DEPTH,
    MAX_NAME_BYTES, MAX_REQUIRE_LEAVES, MAX_RESOURCE_BYTES, MAX_RESOURCE_SEGMENTS,
    MAX_SEGMENT_BYTES,
};
pub use crate::ledger::{
    AuthorizationRequest, DenialReason, ReceiptExpectations, Refusal, SealedKeyRecord,
};

pub mod receipt {
    pub use crate::ledger::{Invalid, ReceiptField};
}

/// Why an accepted protection operation could not produce ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Randomness,
    Encryption,
    /// The recipients named produced no usable path to the payload.
    Unaddressable,
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
        crate::crypto::Failure::Unaddressable => Failure::Unaddressable,
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

/// Seal `message` to a member, bound to `context`.
///
/// The binding rides in the HPKE `info`, so an envelope sealed under one context
/// is a decryption failure under another rather than a policy question. Context
/// parts are length-framed with their count first, so `["ab", "c"]` and
/// `["a", "bc"]` are different bindings.
///
/// The kernel does not own the vocabulary: a caller composes whatever context
/// identifies the thing being sealed. What it must own is a leading part
/// distinct from every other consumer's — the framing removes the concatenation
/// ambiguity, not the naming collision.
pub fn seal_to_bound(
    recipient: &crate::ids::DeviceId,
    context: &[&[u8]],
    message: &[u8],
) -> Result<Option<Vec<u8>>, Failure> {
    crate::crypto::seal_to_bound(recipient, context, message).map_err(map_failure)
}

/// Seal `plaintext` once so that exactly `devices` can read it.
///
/// One random data key encrypts the payload, and each device gets an
/// independent wrap of that key — so admitting a reader is one more wrap and
/// re-encrypts nothing. [`Failure::Unaddressable`] when no device in the set
/// produced a usable wrap: a payload nobody can open is a failure, never a
/// success with an empty result.
pub fn seal_to_devices(
    devices: &[crate::ids::DeviceId],
    context: &[&[u8]],
    plaintext: &[u8],
) -> Result<DeviceSealed, Failure> {
    crate::crypto::seal_to_devices(devices, context, plaintext).map_err(map_failure)
}

/// Admit `newcomer` as a reader, using a device that can already read.
///
/// `false` when `me` cannot open the payload — which is the only way to learn
/// the data key, and therefore the only authority this operation has. The
/// ciphertext is never touched, so every existing wrap stays valid.
pub fn add_device_to_sealed(
    my_seed: &[u8; 32],
    me: &crate::ids::DeviceId,
    context: &[&[u8]],
    sealed: &mut DeviceSealed,
    newcomer: &crate::ids::DeviceId,
) -> Result<bool, Failure> {
    crate::crypto::add_device_to_sealed(my_seed, me, context, sealed, newcomer).map_err(map_failure)
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
