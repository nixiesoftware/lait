//! Exact string domains used at the receiver boundary.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Refusal;

fn is_lower_hex(value: &str, expected_chars: usize) -> bool {
    value.len() == expected_chars
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

macro_rules! hex_id {
    ($name:ident, $chars:expr, $error:literal) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, Refusal> {
                let value = value.into();
                if !is_lower_hex(&value, $chars) {
                    return Err(Refusal::InvalidIdentifier($error));
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(D::Error::custom)
            }
        }
    };
}

/// The coordinator's kinship profile id: `prf_` + 26 Crockford-base32
/// characters — the content address of the identity's genesis, minted by
/// nothing and validated by rehashing wherever the genesis is held.
///
/// Shape-validated here rather than imported: this crate is the language-
/// neutral contract, and six independent implementations validate the same
/// grammar from the fixture, not from a Rust dependency.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoordinatorProfile(String);

impl CoordinatorProfile {
    pub fn parse(value: impl Into<String>) -> Result<Self, Refusal> {
        let value = value.into();
        let valid = value.strip_prefix("prf_").is_some_and(|rest| {
            rest.len() == 26
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'A'..=b'V').contains(&byte))
        });
        if !valid {
            return Err(Refusal::InvalidIdentifier("coordinator profile"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for CoordinatorProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for CoordinatorProfile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for CoordinatorProfile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

hex_id!(DisplayDeviceId, 32, "device id");
hex_id!(DisplayAssignmentId, 32, "assignment id");
hex_id!(DisplayProgramId, 32, "program id");
hex_id!(DisplayPairingId, 32, "pairing id");
hex_id!(RendezvousId, 32, "rendezvous id");
hex_id!(DisplayProgramItemId, 64, "program item id");
hex_id!(DisplayAssetId, 64, "asset id");
hex_id!(ProgramRevision, 64, "program revision");
hex_id!(Sha256Digest, 64, "SHA-256 digest");
hex_id!(CoordinatorFingerprint, 64, "coordinator fingerprint");
hex_id!(Challenge, 64, "challenge");
hex_id!(ReceiverNonce, 64, "receiver nonce");
hex_id!(PollKey, 64, "pairing poll key");
hex_id!(ProofKey, 64, "receiver proof key");
hex_id!(AuthenticationTag, 64, "authentication tag");

macro_rules! visible_debug {
    ($($name:ident),+ $(,)?) => {
        $(
            impl fmt::Debug for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.debug_tuple(stringify!($name)).field(&self.0).finish()
                }
            }
        )+
    };
}

visible_debug!(
    DisplayDeviceId,
    DisplayAssignmentId,
    DisplayProgramId,
    DisplayPairingId,
    RendezvousId,
    DisplayProgramItemId,
    DisplayAssetId,
    ProgramRevision,
    Sha256Digest,
    CoordinatorFingerprint,
    Challenge,
    ReceiverNonce,
    AuthenticationTag,
);

impl fmt::Debug for ProofKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProofKey([REDACTED])")
    }
}

impl fmt::Debug for PollKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PollKey([REDACTED])")
    }
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let high = usize::from(*byte >> 4);
        let low = usize::from(*byte & 0x0f);
        if let (Some(high), Some(low)) = (HEX.get(high), HEX.get(low)) {
            encoded.push(char::from(*high));
            encoded.push(char::from(*low));
        }
    }
    encoded
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0' => Some(0),
        b'1' => Some(1),
        b'2' => Some(2),
        b'3' => Some(3),
        b'4' => Some(4),
        b'5' => Some(5),
        b'6' => Some(6),
        b'7' => Some(7),
        b'8' => Some(8),
        b'9' => Some(9),
        b'a' => Some(10),
        b'b' => Some(11),
        b'c' => Some(12),
        b'd' => Some(13),
        b'e' => Some(14),
        b'f' => Some(15),
        _ => None,
    }
}

pub(crate) fn decode_hex_32(value: &str) -> Result<[u8; 32], Refusal> {
    if !is_lower_hex(value, 64) {
        return Err(Refusal::InvalidEncoding("32-byte lowercase hex"));
    }

    let mut bytes = [0_u8; 32];
    let mut encoded = value.bytes();
    for slot in &mut bytes {
        let high = encoded
            .next()
            .and_then(hex_nibble)
            .ok_or(Refusal::InvalidEncoding("32-byte lowercase hex"))?;
        let low = encoded
            .next()
            .and_then(hex_nibble)
            .ok_or(Refusal::InvalidEncoding("32-byte lowercase hex"))?;
        *slot = (high << 4) | low;
    }
    if encoded.next().is_some() {
        return Err(Refusal::InvalidEncoding("32-byte lowercase hex"));
    }
    Ok(bytes)
}

pub fn random_id_from_bytes(bytes: [u8; 16]) -> String {
    encode_hex(&bytes)
}
