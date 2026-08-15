//! Pairing is receiver enrollment only; assignment is a separate policy act.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bounds::{
    MAX_CHALLENGE_LIFETIME_MS, MAX_CONFIRMATION_PHRASE_WORDS, MAX_LABEL_BYTES,
    MAX_PAIRING_LIFETIME_MS, MAX_RETRY_AFTER_MS,
};
use crate::ids::{
    decode_hex_32, encode_hex, AuthenticationTag, Challenge, CoordinatorFingerprint,
    DisplayDeviceId, DisplayPairingId, PollKey, ProofKey, ReceiverNonce, RendezvousId,
};
use crate::receiver::ReceiverCapabilities;
use crate::wire::Transcript;
use crate::{ProtocolError, PROTOCOL_MAJOR};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoordinatorTrust {
    PinnedCertificate {
        origin: String,
        sha256: CoordinatorFingerprint,
    },
    WebPkiOrigin {
        origin: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorInstance {
    pub protocol_major: u32,
    pub instance: String,
    pub label: String,
    pub trust: CoordinatorTrust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingStartRequest {
    pub protocol_major: u32,
    pub receiver_nonce: ReceiverNonce,
    pub poll_key: PollKey,
    pub rendezvous: Option<RendezvousId>,
    pub capabilities: ReceiverCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingStartResponse {
    pub protocol_major: u32,
    pub pairing: DisplayPairingId,
    pub expires_in_ms: u32,
    pub confirmation_phrase: Vec<String>,
    pub coordinator_fingerprint: CoordinatorFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingStatusRequest {
    pub protocol_major: u32,
    pub pairing: DisplayPairingId,
    pub proof: AuthenticationTag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingRejectionReason {
    UserRejected,
    ControllerUnavailable,
    PolicyRefused,
    FingerprintMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairingStatus {
    Pending {
        retry_after_ms: u32,
    },
    Approved {
        device: DisplayDeviceId,
        proof_key: ProofKey,
        enrollment_challenge: Challenge,
    },
    Rejected {
        reason: PairingRejectionReason,
    },
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCompleteRequest {
    pub protocol_major: u32,
    pub pairing: DisplayPairingId,
    pub device: DisplayDeviceId,
    pub enrollment_challenge: Challenge,
    pub proof: AuthenticationTag,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairingCompleteResponse {
    Enrolled {
        device: DisplayDeviceId,
        next_challenge: Challenge,
    },
    AlreadyEnrolled {
        device: DisplayDeviceId,
        next_challenge: Challenge,
    },
}

fn valid_https_origin(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("https://") else {
        return false;
    };
    !authority.is_empty()
        && authority.len() <= 255
        && authority.is_ascii()
        && !authority.contains(['/', '?', '#', '@'])
}

pub fn validate_instance(instance: &CoordinatorInstance) -> Result<(), ProtocolError> {
    if instance.protocol_major != PROTOCOL_MAJOR {
        return Err(ProtocolError::Unsupported("protocol major"));
    }
    if instance.instance.len() != 32
        || !instance
            .instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::InvalidIdentifier("coordinator instance"));
    }
    if instance.label.is_empty()
        || instance.label.len() > MAX_LABEL_BYTES
        || instance.label.chars().any(char::is_control)
    {
        return Err(ProtocolError::BoundExceeded("coordinator label"));
    }
    let origin = match &instance.trust {
        CoordinatorTrust::PinnedCertificate { origin, .. }
        | CoordinatorTrust::WebPkiOrigin { origin } => origin,
    };
    if !valid_https_origin(origin) {
        return Err(ProtocolError::InvalidShape("coordinator HTTPS origin"));
    }
    Ok(())
}

fn mac_tag(key: &[u8; 32], transcript: &[u8]) -> Result<AuthenticationTag, ProtocolError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| ProtocolError::InvalidEncoding("HMAC-SHA-256 key"))?;
    mac.update(transcript);
    AuthenticationTag::parse(encode_hex(&mac.finalize().into_bytes()))
}

pub fn pairing_status_transcript(pairing: &DisplayPairingId) -> Result<Vec<u8>, ProtocolError> {
    let mut transcript = Transcript::new(b"astrolabe-display/pairing-status/v1")?;
    transcript.u32(PROTOCOL_MAJOR)?;
    transcript.text(pairing.as_str())?;
    Ok(transcript.finish())
}

pub fn authenticate_pairing_status(
    poll_key: &PollKey,
    pairing: &DisplayPairingId,
) -> Result<AuthenticationTag, ProtocolError> {
    let key = decode_hex_32(poll_key.as_str())?;
    mac_tag(&key, &pairing_status_transcript(pairing)?)
}

pub fn pairing_complete_transcript(
    pairing: &DisplayPairingId,
    device: &DisplayDeviceId,
    challenge: &Challenge,
) -> Result<Vec<u8>, ProtocolError> {
    let mut transcript = Transcript::new(b"astrolabe-display/pairing-complete/v1")?;
    transcript.u32(PROTOCOL_MAJOR)?;
    transcript.text(pairing.as_str())?;
    transcript.text(device.as_str())?;
    transcript.text(challenge.as_str())?;
    Ok(transcript.finish())
}

pub fn authenticate_pairing_complete(
    proof_key: &ProofKey,
    pairing: &DisplayPairingId,
    device: &DisplayDeviceId,
    challenge: &Challenge,
) -> Result<AuthenticationTag, ProtocolError> {
    let key = decode_hex_32(proof_key.as_str())?;
    mac_tag(
        &key,
        &pairing_complete_transcript(pairing, device, challenge)?,
    )
}

const CONFIRMATION_WORDS: [&str; 32] = [
    "amber", "anchor", "apple", "beacon", "birch", "cedar", "comet", "coral", "delta", "ember",
    "falcon", "fjord", "garden", "harbor", "hazel", "indigo", "juniper", "lantern", "maple",
    "meadow", "meteor", "olive", "orbit", "pebble", "quartz", "river", "saffron", "signal",
    "spruce", "violet", "willow", "zephyr",
];

pub fn confirmation_phrase(
    fingerprint: &CoordinatorFingerprint,
    pairing: &DisplayPairingId,
    receiver_nonce: &ReceiverNonce,
) -> Result<Vec<String>, ProtocolError> {
    let mut transcript = Transcript::new(b"astrolabe-display/confirmation-phrase/v1")?;
    transcript.u32(PROTOCOL_MAJOR)?;
    transcript.text(fingerprint.as_str())?;
    transcript.text(pairing.as_str())?;
    transcript.text(receiver_nonce.as_str())?;
    let digest = Sha256::digest(transcript.finish());
    let words = digest
        .iter()
        .take(MAX_CONFIRMATION_PHRASE_WORDS)
        .map(|byte| {
            let index = usize::from(*byte & 0x1f);
            CONFIRMATION_WORDS
                .get(index)
                .copied()
                .ok_or(ProtocolError::InvalidShape("confirmation word index"))
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if words.len() != MAX_CONFIRMATION_PHRASE_WORDS {
        return Err(ProtocolError::InvalidShape("confirmation phrase"));
    }
    Ok(words)
}

pub fn validate_pairing_start_response(
    response: &PairingStartResponse,
) -> Result<(), ProtocolError> {
    if response.protocol_major != PROTOCOL_MAJOR {
        return Err(ProtocolError::Unsupported("protocol major"));
    }
    if response.expires_in_ms == 0 || response.expires_in_ms > MAX_PAIRING_LIFETIME_MS {
        return Err(ProtocolError::BoundExceeded("pairing lifetime"));
    }
    if response.confirmation_phrase.len() != MAX_CONFIRMATION_PHRASE_WORDS
        || response.confirmation_phrase.iter().any(|word| {
            !CONFIRMATION_WORDS.contains(&word.as_str())
                || word.len() > 16
                || !word.bytes().all(|byte| byte.is_ascii_lowercase())
        })
    {
        return Err(ProtocolError::InvalidShape("confirmation phrase"));
    }
    Ok(())
}

pub fn validate_pairing_status(status: &PairingStatus) -> Result<(), ProtocolError> {
    if let PairingStatus::Pending { retry_after_ms } = status {
        if *retry_after_ms == 0 || *retry_after_ms > MAX_RETRY_AFTER_MS {
            return Err(ProtocolError::BoundExceeded("pairing retry interval"));
        }
    }
    Ok(())
}

pub fn validate_challenge_lifetime(expires_in_ms: u32) -> Result<(), ProtocolError> {
    if expires_in_ms == 0 || expires_in_ms > MAX_CHALLENGE_LIFETIME_MS {
        return Err(ProtocolError::BoundExceeded("challenge lifetime"));
    }
    Ok(())
}
