//! Protection for canonical Fabric artifacts and the key-source seam Replica
//! consults to seal/open them.
//!
//! Each persisted artifact envelope is exactly `epoch_id[16] || nonce[12] ||
//! ciphertext_and_tag` (the existing construction — no new cryptography), produced by
//! [`mechanics::authorization::body_seal`] under an opaque
//! [`mechanics::authorization::AuthorizedBodyKey`] capability that mechanics-side
//! policy mints. Replica selects the capability under Space policy and passes
//! it only to seal/open; nothing here decides epoch legitimacy.

use fabric::Artifact;
use mechanics::authorization::{AuthorizedBodyKey, BODY_ENVELOPE_OVERHEAD, BODY_EPOCH_ID_LEN};

/// The maximum protected Body envelope size (64 MiB) — the per-Body bound,
/// checked before allocation on both seal and open.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// The maximum canonical plaintext size: the envelope bound minus the fixed
/// envelope overhead, so a maximal plaintext still seals within
/// [`MAX_BODY_BYTES`].
pub const MAX_PROTECTED_PLAINTEXT: usize = MAX_BODY_BYTES - BODY_ENVELOPE_OVERHEAD;

/// The declared mutation model tags.
pub const MUTATION_ATOMIC: u8 = 1;
pub const MUTATION_COLLABORATIVE: u8 = 2;

/// Why a protected payload failed. Commitment and AEAD failures share
/// [`Invalid::InvalidProtectedBody`] deliberately — no oracle
/// distinguishes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// Malformed envelope, wrong key, failed authentication, non-canonical
    /// plaintext, or a model/variant disagreement.
    InvalidProtectedBody,
    /// The plaintext or envelope exceeds the Body maximum.
    BodyTooLarge,
    /// An accepted local protection operation could not obtain entropy or
    /// produce ciphertext.
    Protection(mechanics::authorization::Failure),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Invalid {}

/// Seal one canonical Fabric artifact under the same authorized Body-key
/// capability as the live interchange envelope. The artifact already carries
/// its own causal format version, so this protection layer adds no second
/// causal envelope or vocabulary.
pub(crate) fn seal_artifact(
    artifact: &Artifact,
    key: &AuthorizedBodyKey,
) -> Result<Vec<u8>, Invalid> {
    let plaintext = artifact.encode();
    if plaintext.len() > MAX_PROTECTED_PLAINTEXT {
        return Err(Invalid::BodyTooLarge);
    }
    mechanics::authorization::body_seal(key, &plaintext).map_err(Invalid::Protection)
}

/// Open and canonically decode one protected Fabric artifact.
pub(crate) fn open_artifact(key: &AuthorizedBodyKey, envelope: &[u8]) -> Result<Artifact, Invalid> {
    if envelope.len() > MAX_BODY_BYTES {
        return Err(Invalid::BodyTooLarge);
    }
    if envelope.len() < BODY_EPOCH_ID_LEN {
        return Err(Invalid::InvalidProtectedBody);
    }
    let plaintext =
        mechanics::authorization::body_open(key, envelope).ok_or(Invalid::InvalidProtectedBody)?;
    Artifact::decode_canonical(&plaintext).map_err(|_| Invalid::InvalidProtectedBody)
}

/// The mechanics-owned key seam Replica consults. The composition root
/// implements it over the authorized epoch set (signed history); Replica calls
/// it under Space policy and never persists or exposes key material.
pub trait BodyKeySource: Send + Sync {
    /// The capability for sealing **new** local material: the current
    /// authorized epoch's key. `None` when no authorized epoch key is held;
    /// Replica then returns its typed `BodyKeyUnavailable` failure.
    fn sealing_key(&self) -> Option<AuthorizedBodyKey>;

    /// The capability for opening material sealed under `epoch`. `None` when
    /// that epoch's key is not held (the opaque branch) or the epoch is not
    /// authorized (rejected upstream).
    fn opening_key(&self, epoch: &[u8; BODY_EPOCH_ID_LEN]) -> Option<AuthorizedBodyKey>;
}

/// A single static epoch key source for tests and single-epoch deployments.
pub struct StaticBodyKeys {
    key: AuthorizedBodyKey,
}

impl StaticBodyKeys {
    pub fn new(key: AuthorizedBodyKey) -> Self {
        Self { key }
    }
}

impl BodyKeySource for StaticBodyKeys {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        Some(self.key.clone())
    }
    fn opening_key(&self, epoch: &[u8; BODY_EPOCH_ID_LEN]) -> Option<AuthorizedBodyKey> {
        (self.key.epoch_id() == epoch).then(|| self.key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AuthorizedBodyKey {
        AuthorizedBodyKey::for_authorized_epoch([7u8; 16], [9u8; 32])
    }

    #[test]
    fn causal_artifacts_use_the_same_protection_boundary() {
        let artifact = Artifact::Replace {
            format_version: fabric::CAUSAL_FORMAT_VERSION,
            bytes: b"artifact plaintext".to_vec(),
        };
        let envelope = seal_artifact(&artifact, &key()).unwrap();
        assert_eq!(open_artifact(&key(), &envelope).unwrap(), artifact);
        assert!(!envelope
            .windows(b"artifact plaintext".len())
            .any(|window| window == b"artifact plaintext"));

        let mut tampered = envelope;
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        assert_eq!(
            open_artifact(&key(), &tampered),
            Err(Invalid::InvalidProtectedBody)
        );
    }

    #[test]
    fn bounds_are_checked_before_allocation() {
        // An over-bound input is refused by length alone (no decode attempt).
        let huge_envelope = vec![0u8; MAX_BODY_BYTES + 1];
        assert_eq!(
            open_artifact(&key(), &huge_envelope),
            Err(Invalid::BodyTooLarge)
        );
    }

    #[test]
    fn the_static_key_source_serves_exactly_its_epoch() {
        let src = StaticBodyKeys::new(key());
        assert!(src.sealing_key().is_some());
        assert!(src.opening_key(&[7u8; 16]).is_some());
        assert!(src.opening_key(&[8u8; 16]).is_none());
    }
}
