//! The wire requests, their preimages, and the client half that mints them.
//!
//! Both halves live here for the reason `lait_post::sign` gives: a caller that
//! composed its own preimage would be reimplementing a format whose only other
//! implementation is the verifier a few lines up, and the failure mode is a
//! signature that verifies nowhere, reported as "bad signature", with the actual
//! disagreement invisible.
//!
//! **Signing takes a seed, and that does not make the directory hold keys.** The
//! service — [`crate::Service`], [`crate::Store`], [`crate::http::router`] —
//! never sees one. These are functions a *client* calls in its own process, in
//! this file only because the wire format is.

use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};

use crate::{address::Address, Refusal};

/// Domain for the statement a publisher signs.
const PUBLISH_DOMAIN: &[u8] = b"lait/directory/1/publish";
/// Domain for the statement an asker signs. Distinct from publishing, so a
/// captured resolution can never be re-presented as a publication.
const RESOLVE_DOMAIN: &[u8] = b"lait/directory/1/resolve";

pub(crate) fn framed(out: &mut Vec<u8>, part: &[u8]) {
    // Truncation is unreachable: every part is a device id, a nonce, an address
    // or an announcement, each bounded far below 4 GiB by the time it arrives.
    // Saturating rather than casting, so the bound is expressed rather than
    // assumed by whoever reads this next.
    out.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(part);
}

/// A single-use nonce the service issued, and who it was issued to.
///
/// Answered rather than re-derived. The nonce is remembered at the service
/// exactly once, so a client that invented one would be signing something
/// nobody will accept — which is the whole point, since a bare signature is
/// replayable by anyone who observed it (AUTH-16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub device: DeviceId,
    #[serde(with = "hex_nonce")]
    pub nonce: [u8; 32],
    pub issued_at: u64,
}

/// Publish a profile's signed device events.
///
/// `announcement` is carried as its own encoded bytes rather than as a decoded
/// value, so the signature covers exactly what the publisher meant and a
/// re-encoding here cannot invalidate it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPublish {
    /// The device presenting this. It must be one the announcement avows, which
    /// is checked against the announcement rather than taken on trust.
    pub device: DeviceId,
    #[serde(with = "hex_bytes")]
    pub announcement: Vec<u8>,
    #[serde(with = "hex_nonce")]
    pub nonce: [u8; 32],
    #[serde(with = "hex_signature")]
    pub signature: [u8; 64],
}

impl SignedPublish {
    pub(crate) fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128 + self.announcement.len());
        framed(&mut out, PUBLISH_DOMAIN);
        framed(&mut out, self.device.as_str().as_bytes());
        framed(&mut out, &self.nonce);
        framed(&mut out, &self.announcement);
        out
    }
}

/// Resolve one exact address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedResolve {
    /// The asker. Rate limits have a subject because of this field, and it is
    /// also why resolution is never anonymous even though it is never an account.
    pub device: DeviceId,
    /// The address being asked about, as typed.
    pub address: String,
    #[serde(with = "hex_nonce")]
    pub nonce: [u8; 32],
    #[serde(with = "hex_signature")]
    pub signature: [u8; 64],
}

impl SignedResolve {
    pub(crate) fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        framed(&mut out, RESOLVE_DOMAIN);
        framed(&mut out, self.device.as_str().as_bytes());
        framed(&mut out, &self.nonce);
        framed(&mut out, self.address.as_bytes());
        out
    }
}

/// Check a signature against a device's own key.
///
/// The device id **is** the key, so there is no lookup and nothing to be
/// out of date about — which is what lets this service verify everything it
/// accepts while holding nothing.
pub(crate) fn verify(
    device: &DeviceId,
    preimage: &[u8],
    signature: &[u8; 64],
) -> Result<(), Refusal> {
    let key = canonical_key(device)?;
    if mechanics::actor::verify_detached(&key, preimage, signature) {
        Ok(())
    } else {
        Err(Refusal::NotAuthentic)
    }
}

/// A device id's key bytes, insisting on the canonical spelling.
///
/// `DeviceId` compares spelling-blind, so a re-spelling would resolve correctly
/// in a map — but this service's store keys a document on the *string*, where no
/// `Eq` impl can help. Refusing at the boundary is what the Post does, for the
/// same reason.
pub(crate) fn canonical_key(device: &DeviceId) -> Result<[u8; 32], Refusal> {
    let parsed = DeviceId::parse(device.as_str()).ok_or(Refusal::Malformed)?;
    if parsed.as_str() != device.as_str() {
        return Err(Refusal::Malformed);
    }
    parsed.key_bytes().ok_or(Refusal::Malformed)
}

/// Build the two signed requests, as a client.
pub mod sign {
    use super::{Challenge, SignedPublish, SignedResolve};
    use crate::address::Address;

    /// Publish `announcement` as `seed`'s device, answering `challenge`.
    #[must_use]
    pub fn publish(seed: &[u8; 32], challenge: &Challenge, announcement: Vec<u8>) -> SignedPublish {
        let mut request = SignedPublish {
            device: challenge.device.clone(),
            announcement,
            nonce: challenge.nonce,
            signature: [0u8; 64],
        };
        // Built from the finished value rather than from the arguments, so a
        // field added later is covered by default instead of silently left out.
        request.signature = mechanics::actor::sign_detached(seed, &request.preimage());
        request
    }

    /// Ask about exactly one address, as `seed`'s device.
    #[must_use]
    pub fn resolve(seed: &[u8; 32], challenge: &Challenge, address: &Address) -> SignedResolve {
        let mut request = SignedResolve {
            device: challenge.device.clone(),
            address: address.as_str().to_owned(),
            nonce: challenge.nonce,
            signature: [0u8; 64],
        };
        request.signature = mechanics::actor::sign_detached(seed, &request.preimage());
        request
    }
}

/// The address a request names, parsed. Kept here so the one place that turns
/// wire text into an [`Address`] is beside the wire type.
impl SignedResolve {
    pub(crate) fn parsed_address(&self) -> Result<Address, Refusal> {
        Address::parse(&self.address)
    }
}

mod hex_nonce {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let raw = data_encoding::HEXLOWER_PERMISSIVE
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)?;
        raw.try_into()
            .map_err(|_| serde::de::Error::custom("a nonce is 32 bytes"))
    }
}

pub(crate) mod hex_signature {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let text = String::deserialize(d)?;
        let raw = data_encoding::HEXLOWER_PERMISSIVE
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)?;
        raw.try_into()
            .map_err(|_| serde::de::Error::custom("a signature is 64 bytes"))
    }
}

pub(crate) mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        data_encoding::HEXLOWER_PERMISSIVE
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}
