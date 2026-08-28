//! Pairing is receiver enrollment only; assignment is a separate policy act.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bounds::{
    MAX_CERTIFICATE_PEM_BYTES, MAX_CHALLENGE_LIFETIME_MS, MAX_CONFIRMATION_PHRASE_WORDS,
    MAX_LABEL_BYTES, MAX_PAIRING_LIFETIME_MS, MAX_RETRY_AFTER_MS,
};
use crate::ids::{
    decode_hex_32, encode_hex, AuthenticationTag, Challenge, CoordinatorFingerprint,
    CoordinatorProfile, DisplayDeviceId, DisplayPairingId, PollKey, ProofKey, ReceiverNonce,
    RendezvousId,
};
use crate::receiver::ReceiverCapabilities;
use crate::wire::Transcript;
use crate::{Refusal, PROTOCOL_MAJOR};

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
    /// Anchored on the coordinator's *identity* rather than on any placement.
    ///
    /// The origin is a resolved route — where the identity answers right now,
    /// typically through a Web-PKI fronting router — and may change without
    /// the receiver re-pairing. The profile is what the receiver holds the
    /// coordinator to: the instance must report it, and the confirmation
    /// phrase derives from it, so the six words a person compares prove *who*
    /// and not *where*.
    Profile {
        origin: String,
        profile: CoordinatorProfile,
    },
}

/// Non-secret trust material handed to a receiver before it opens a network
/// connection. Self-hosted coordinators use the pinned-certificate variant;
/// hosted coordinators may use the platform Web PKI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverBootstrap {
    pub protocol_major: u32,
    pub trust: CoordinatorTrust,
    /// The exact public leaf certificate for a pinned coordinator. Platforms
    /// such as Roku need the PEM itself to construct a request-local trust
    /// store; its SHA-256 must equal the fingerprint in `trust`.
    pub certificate_pem: Option<String>,
    pub rendezvous: Option<RendezvousId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorInstance {
    pub protocol_major: u32,
    pub instance: String,
    pub label: String,
    /// The identity this placement answers for. Every placement of one
    /// identity reports the same profile; `instance` is what distinguishes
    /// them.
    pub profile: CoordinatorProfile,
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
    /// The identity the phrase derives from — what enrollment anchors on.
    pub coordinator_profile: CoordinatorProfile,
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

pub fn validate_bootstrap(bootstrap: &ReceiverBootstrap) -> Result<(), Refusal> {
    if bootstrap.protocol_major != PROTOCOL_MAJOR {
        return Err(Refusal::Unsupported("protocol major"));
    }
    let origin = match &bootstrap.trust {
        CoordinatorTrust::PinnedCertificate { origin, sha256 } => {
            let pem = bootstrap
                .certificate_pem
                .as_deref()
                .ok_or(Refusal::InvalidShape("pinned certificate PEM"))?;
            let certificate = decode_certificate_pem(pem)?;
            let digest = Sha256::digest(certificate);
            let actual = data_encoding::HEXLOWER.encode(&digest);
            if actual != sha256.as_str() {
                return Err(Refusal::Integrity("pinned certificate fingerprint"));
            }
            origin
        }
        CoordinatorTrust::WebPkiOrigin { origin } => {
            if bootstrap.certificate_pem.is_some() {
                return Err(Refusal::InvalidShape("Web PKI certificate PEM"));
            }
            origin
        }
        // Identity-anchored: the origin is a resolved route over the platform
        // Web PKI, so pinned material would claim an authority the anchor
        // deliberately does not rest on. The profile's own shape is enforced
        // by its type; there is nothing further to check until the instance
        // reports one, which `DisplayReceiverClient` compares at first
        // contact.
        CoordinatorTrust::Profile { origin, .. } => {
            if bootstrap.certificate_pem.is_some() {
                return Err(Refusal::InvalidShape("profile-anchored certificate PEM"));
            }
            origin
        }
    };
    if !valid_https_origin(origin) {
        return Err(Refusal::InvalidShape("coordinator HTTPS origin"));
    }
    Ok(())
}

/// Decode the single canonical PEM certificate carried by a pinned bootstrap.
/// Certificate chains and unrelated PEM blocks are deliberately refused.
pub fn decode_certificate_pem(pem: &str) -> Result<Vec<u8>, Refusal> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----\n";
    const END: &str = "-----END CERTIFICATE-----\n";
    if pem.is_empty()
        || pem.len() > MAX_CERTIFICATE_PEM_BYTES
        || !pem.is_ascii()
        || !pem.starts_with(BEGIN)
        || !pem.ends_with(END)
    {
        return Err(Refusal::InvalidEncoding("pinned certificate PEM"));
    }
    let body = pem
        .strip_prefix(BEGIN)
        .and_then(|value| value.strip_suffix(END))
        .ok_or(Refusal::InvalidEncoding("pinned certificate PEM"))?;
    if body.is_empty() || body.lines().any(|line| line.is_empty() || line.len() > 64) {
        return Err(Refusal::InvalidEncoding("pinned certificate PEM"));
    }
    let encoded: String = body.lines().collect();
    let certificate = data_encoding::BASE64
        .decode(encoded.as_bytes())
        .map_err(|_| Refusal::InvalidEncoding("pinned certificate PEM"))?;
    if certificate.is_empty() || certificate.len() > MAX_CERTIFICATE_PEM_BYTES {
        return Err(Refusal::BoundExceeded("pinned certificate"));
    }
    Ok(certificate)
}

pub fn validate_instance(instance: &CoordinatorInstance) -> Result<(), Refusal> {
    if instance.protocol_major != PROTOCOL_MAJOR {
        return Err(Refusal::Unsupported("protocol major"));
    }
    if instance.instance.len() != 32
        || !instance
            .instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Refusal::InvalidIdentifier("coordinator instance"));
    }
    if instance.label.is_empty()
        || instance.label.len() > MAX_LABEL_BYTES
        || instance.label.chars().any(char::is_control)
    {
        return Err(Refusal::BoundExceeded("coordinator label"));
    }
    let origin = match &instance.trust {
        CoordinatorTrust::PinnedCertificate { origin, .. }
        | CoordinatorTrust::WebPkiOrigin { origin } => origin,
        CoordinatorTrust::Profile { origin, profile } => {
            // The instance's own profile and its trust anchor cannot disagree
            // about who this is — a placement reporting one identity while
            // anchored on another is two coordinators wearing one route.
            if profile != &instance.profile {
                return Err(Refusal::Integrity("coordinator profile anchor"));
            }
            origin
        }
    };
    if !valid_https_origin(origin) {
        return Err(Refusal::InvalidShape("coordinator HTTPS origin"));
    }
    Ok(())
}

fn mac_tag(key: &[u8; 32], transcript: &[u8]) -> Result<AuthenticationTag, Refusal> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| Refusal::InvalidEncoding("HMAC-SHA-256 key"))?;
    mac.update(transcript);
    AuthenticationTag::parse(encode_hex(&mac.finalize().into_bytes()))
}

pub fn pairing_status_transcript(pairing: &DisplayPairingId) -> Result<Vec<u8>, Refusal> {
    let mut transcript = Transcript::new(b"astrolabe-display/pairing-status/v1")?;
    transcript.u32(PROTOCOL_MAJOR)?;
    transcript.text(pairing.as_str())?;
    Ok(transcript.finish())
}

pub fn authenticate_pairing_status(
    poll_key: &PollKey,
    pairing: &DisplayPairingId,
) -> Result<AuthenticationTag, Refusal> {
    let key = decode_hex_32(poll_key.as_str())?;
    mac_tag(&key, &pairing_status_transcript(pairing)?)
}

pub fn pairing_complete_transcript(
    pairing: &DisplayPairingId,
    device: &DisplayDeviceId,
    challenge: &Challenge,
) -> Result<Vec<u8>, Refusal> {
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
) -> Result<AuthenticationTag, Refusal> {
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

/// The symbols a rendezvous code is drawn from: Crockford's base32.
///
/// Chosen for a person reading a code off one screen and entering it on a
/// television with a remote. No `I`, `L`, `O` or `U`, so nothing looks like a
/// digit or reads as a word; what someone types as `O` or `l` is normalised
/// to the digit it resembles rather than refused.
pub const RENDEZVOUS_CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// How many symbols a rendezvous code has: forty bits, against a code that
/// lives minutes and is spent by its first use.
pub const RENDEZVOUS_CODE_CHARS: usize = 8;

/// The code as a person entered it, reduced to the eight symbols it names.
///
/// Case, spaces and the grouping hyphen are the operator's; the confusable
/// letters fold onto the digits they resemble. Anything else is a code that
/// was not read correctly, and is refused rather than guessed at.
pub fn normalize_rendezvous_code(entered: &str) -> Result<String, Refusal> {
    let mut code = String::with_capacity(RENDEZVOUS_CODE_CHARS);
    for character in entered.chars() {
        let symbol = match character {
            ' ' | '-' | '_' | '.' => continue,
            'o' | 'O' => '0',
            'i' | 'I' | 'l' | 'L' => '1',
            other => other.to_ascii_uppercase(),
        };
        let byte =
            u8::try_from(symbol).map_err(|_| Refusal::InvalidIdentifier("rendezvous code"))?;
        if !RENDEZVOUS_CODE_ALPHABET.contains(&byte) {
            return Err(Refusal::InvalidIdentifier("rendezvous code"));
        }
        code.push(symbol);
    }
    if code.len() != RENDEZVOUS_CODE_CHARS {
        return Err(Refusal::InvalidIdentifier("rendezvous code"));
    }
    Ok(code)
}

/// The code grouped for reading: `XXXX-XXXX`.
pub fn group_rendezvous_code(code: &str) -> Result<String, Refusal> {
    let code = normalize_rendezvous_code(code)?;
    let mut grouped = String::with_capacity(RENDEZVOUS_CODE_CHARS + 1);
    for (index, symbol) in code.chars().enumerate() {
        if index == RENDEZVOUS_CODE_CHARS / 2 {
            grouped.push('-');
        }
        grouped.push(symbol);
    }
    Ok(grouped)
}

/// The rendezvous id a code names on the wire.
///
/// The wire carries a 32-hex identifier, which nobody should have to type;
/// the code is what a person carries between screens. Deriving one from the
/// other keeps the wire shape every receiver already accepts, and keeps the
/// coordinator's table keyed by something that is not the secret itself. It
/// is a transcript digest like the phrase, and for the same reason: its
/// meaning must not depend on any platform's string handling.
pub fn rendezvous_from_code(entered: &str) -> Result<RendezvousId, Refusal> {
    let code = normalize_rendezvous_code(entered)?;
    let mut transcript = Transcript::new(b"astrolabe-display/rendezvous/v1")?;
    transcript.u32(PROTOCOL_MAJOR)?;
    transcript.text(&code)?;
    let digest = Sha256::digest(transcript.finish());
    let leading = digest
        .get(..16)
        .ok_or(Refusal::InvalidShape("rendezvous digest"))?;
    RendezvousId::parse(encode_hex(leading))
}

pub fn confirmation_phrase(
    profile: &CoordinatorProfile,
    pairing: &DisplayPairingId,
    receiver_nonce: &ReceiverNonce,
) -> Result<Vec<String>, Refusal> {
    // v2: the phrase commits the *identity*, not a placement's certificate.
    // The words a person compares must stay the same when the coordinator
    // moves machines or rotates a certificate — a placement is not what is
    // being enrolled against.
    let mut transcript = Transcript::new(b"astrolabe-display/confirmation-phrase/v2")?;
    transcript.u32(PROTOCOL_MAJOR)?;
    transcript.text(profile.as_str())?;
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
                .ok_or(Refusal::InvalidShape("confirmation word index"))
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if words.len() != MAX_CONFIRMATION_PHRASE_WORDS {
        return Err(Refusal::InvalidShape("confirmation phrase"));
    }
    Ok(words)
}

pub fn validate_pairing_start_response(response: &PairingStartResponse) -> Result<(), Refusal> {
    if response.protocol_major != PROTOCOL_MAJOR {
        return Err(Refusal::Unsupported("protocol major"));
    }
    if response.expires_in_ms == 0 || response.expires_in_ms > MAX_PAIRING_LIFETIME_MS {
        return Err(Refusal::BoundExceeded("pairing lifetime"));
    }
    if response.confirmation_phrase.len() != MAX_CONFIRMATION_PHRASE_WORDS
        || response.confirmation_phrase.iter().any(|word| {
            !CONFIRMATION_WORDS.contains(&word.as_str())
                || word.len() > 16
                || !word.bytes().all(|byte| byte.is_ascii_lowercase())
        })
    {
        return Err(Refusal::InvalidShape("confirmation phrase"));
    }
    Ok(())
}

pub fn validate_pairing_status(status: &PairingStatus) -> Result<(), Refusal> {
    if let PairingStatus::Pending { retry_after_ms } = status {
        if *retry_after_ms == 0 || *retry_after_ms > MAX_RETRY_AFTER_MS {
            return Err(Refusal::BoundExceeded("pairing retry interval"));
        }
    }
    Ok(())
}

pub fn validate_challenge_lifetime(expires_in_ms: u32) -> Result<(), Refusal> {
    if expires_in_ms == 0 || expires_in_ms > MAX_CHALLENGE_LIFETIME_MS {
        return Err(Refusal::BoundExceeded("challenge lifetime"));
    }
    Ok(())
}
