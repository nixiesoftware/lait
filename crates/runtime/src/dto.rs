//! External DTOs — the language-neutral JSON surface an independent,
//! Product-neutral consumer speaks to the orbital runtime (C6.1).
//!
//! These are **not** the signed/internal protocol (that stays postcard and is
//! never exposed as a language DTO). They are versioned JSON objects carrying an
//! explicit `protocolVersion`, with **strict unknown-field rejection**
//! (`deny_unknown_fields`), bounded strings, and validated encodings: base64
//! and hex fields are checked for syntax **and decoded length**, not only
//! source-string length. The committed JSON Schema 2020-12
//! (`crates/runtime/schema/dto.schema.json`) is generated from these Rust
//! types and drift-checked by `tests/dto_schema.rs`, together with canonical
//! positive and negative examples validated in both directions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The DTO protocol version. Bumped only on a breaking DTO change; a consumer
/// sending another version is rejected, never negotiated (clean-break formats).
pub const DTO_PROTOCOL_VERSION: u32 = 2;

/// The maximum length of a DTO identifier string (World/schema ids, request
/// ids). Bounds every string the surface accepts.
pub const MAX_DTO_STRING: usize = 256;

/// The maximum decoded payload/effect/projection bytes — checked on the
/// DECODED length, not the base64 text length. The generic transport must be
/// large enough to carry any World payload admitted by Runtime; product
/// declarations may only tighten this outer ceiling.
pub const MAX_DTO_PAYLOAD: usize = crate::world::MAX_PAYLOAD_BYTES as usize;

/// Why a DTO failed validation beyond JSON structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// The bytes were not the expected JSON shape (or carried unknown fields).
    Malformed,
    /// `protocolVersion` was not [`DTO_PROTOCOL_VERSION`].
    UnsupportedProtocol(u32),
    /// A bounded string exceeded [`MAX_DTO_STRING`].
    StringTooLong,
    /// A base64 field did not decode.
    BadBase64,
    /// A hex field did not decode or had the wrong decoded length.
    BadHex,
    /// A decoded payload exceeded [`MAX_DTO_PAYLOAD`].
    PayloadTooLarge,
    /// An identifier failed its grammar (World/schema id form).
    BadIdentifier,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Invalid {}

fn check_len(s: &str) -> Result<(), Invalid> {
    (s.len() <= MAX_DTO_STRING)
        .then_some(())
        .ok_or(Invalid::StringTooLong)
}

fn check_b64_payload(s: &str) -> Result<(), Invalid> {
    let decoded = data_encoding::BASE64
        .decode(s.as_bytes())
        .map_err(|_| Invalid::BadBase64)?;
    (decoded.len() <= MAX_DTO_PAYLOAD)
        .then_some(())
        .ok_or(Invalid::PayloadTooLarge)
}

fn check_hex32(s: &str) -> Result<(), Invalid> {
    let decoded = data_encoding::HEXLOWER
        .decode(s.as_bytes())
        .map_err(|_| Invalid::BadHex)?;
    (decoded.len() == 32).then_some(()).ok_or(Invalid::BadHex)
}

fn check_hex16(s: &str) -> Result<(), Invalid> {
    let decoded = data_encoding::HEXLOWER
        .decode(s.as_bytes())
        .map_err(|_| Invalid::BadHex)?;
    (decoded.len() == 16).then_some(()).ok_or(Invalid::BadHex)
}

fn check_world(s: &str) -> Result<(), Invalid> {
    check_len(s)?;
    replica::body::WorldId::parse(s)
        .map(|_| ())
        .ok_or(Invalid::BadIdentifier)
}

fn check_schema_id(s: &str) -> Result<(), Invalid> {
    check_len(s)?;
    replica::body::SchemaId::parse(s)
        .map(|_| ())
        .ok_or(Invalid::BadIdentifier)
}

fn check_version(v: u32) -> Result<(), Invalid> {
    (v == DTO_PROTOCOL_VERSION)
        .then_some(())
        .ok_or(Invalid::UnsupportedProtocol(v))
}

macro_rules! json_codec {
    ($ty:ty) => {
        impl $ty {
            /// Encode to canonical JSON bytes.
            pub fn to_json(&self) -> Vec<u8> {
                serde_json::to_vec(self).expect("serialize dto")
            }

            /// Decode from JSON, rejecting unknown fields, foreign protocol
            /// versions, over-long strings, and malformed/over-long encodings.
            pub fn from_json(bytes: &[u8]) -> Result<Self, Invalid> {
                let dto: Self = serde_json::from_slice(bytes).map_err(|_| Invalid::Malformed)?;
                dto.validate()?;
                Ok(dto)
            }
        }
    };
}

/// A submit request DTO. The application `payload` is base64 in JSON (arbitrary
/// bytes are not JSON-safe); everything else is a bounded string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SubmitRequestDto {
    pub protocol_version: u32,
    pub world: String,
    pub schema: String,
    pub schema_version: u32,
    /// Hex (lowercase) of the caller's 16-byte request id (the idempotency
    /// scope component).
    pub request_id_hex: String,
    /// Base64 (standard, padded) of the application intent bytes.
    pub payload_b64: String,
}

impl SubmitRequestDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)?;
        check_world(&self.world)?;
        check_schema_id(&self.schema)?;
        check_len(&self.request_id_hex)?;
        check_hex16(&self.request_id_hex)?;
        check_b64_payload(&self.payload_b64)
    }
}
json_codec!(SubmitRequestDto);

/// A **signed** submit DTO: the canonical signed World action (postcard bytes,
/// opaque, produced and verified only by the Rust protocol layer) together
/// with the coordinates a consumer can read without decoding postcard. Every
/// spelled coordinate is validated against its identifier grammar; the signed
/// bytes are size-bounded and never interpreted at this layer — signature and
/// header verification happen in [`crate::action`], not in JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SignedSubmitDto {
    pub protocol_version: u32,
    /// The Space the action addresses (`ws_` + 26 base32 chars).
    pub space_id: String,
    /// The World the action addresses (reverse-domain form).
    pub world: String,
    /// The acting principal (`act_` + 64 lowercase hex chars).
    pub actor_id: String,
    /// The signing device key (64 lowercase hex chars).
    pub device_hex: String,
    /// Hex (lowercase) of the action's 16-byte request id.
    pub request_id_hex: String,
    /// Base64 (standard, padded) of the complete signed action bytes.
    pub signed_action_b64: String,
}

impl SignedSubmitDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)?;
        check_len(&self.space_id)?;
        mechanics::ids::SpaceId::parse(&self.space_id).ok_or(Invalid::BadIdentifier)?;
        check_world(&self.world)?;
        check_len(&self.actor_id)?;
        mechanics::ids::ActorId::parse(&self.actor_id).ok_or(Invalid::BadIdentifier)?;
        check_len(&self.device_hex)?;
        mechanics::ids::DeviceId::parse(&self.device_hex).ok_or(Invalid::BadIdentifier)?;
        check_len(&self.request_id_hex)?;
        check_hex16(&self.request_id_hex)?;
        check_b64_payload(&self.signed_action_b64)
    }
}
json_codec!(SignedSubmitDto);

/// Portable semantic World publication coordinate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PublicationIdDto {
    pub manifest_root_hex: String,
    pub implementation_digest_hex: String,
    pub extractor_schema_digest_hex: String,
}

impl PublicationIdDto {
    fn validate(&self) -> Result<(), Invalid> {
        for value in [
            &self.manifest_root_hex,
            &self.implementation_digest_hex,
            &self.extractor_schema_digest_hex,
        ] {
            check_len(value)?;
            check_hex32(value)?;
        }
        Ok(())
    }
}

/// Complete Station-local read publication coordinate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorldPublicationIdDto {
    pub publication: PublicationIdDto,
    pub materialization: u64,
}

impl WorldPublicationIdDto {
    fn validate(&self) -> Result<(), Invalid> {
        self.publication.validate()?;
        if self.materialization == 0 {
            return Err(Invalid::BadIdentifier);
        }
        Ok(())
    }
}

/// A query request DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueryRequestDto {
    pub protocol_version: u32,
    pub world: String,
    pub schema: String,
    pub schema_version: u32,
    pub payload_b64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<PublicationIdDto>,
}

impl QueryRequestDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)?;
        check_world(&self.world)?;
        check_schema_id(&self.schema)?;
        check_b64_payload(&self.payload_b64)?;
        if let Some(publication) = &self.publication {
            publication.validate()?;
        }
        Ok(())
    }
}
json_codec!(QueryRequestDto);

/// A submit response DTO: the committed effect, frontier, and touched scopes.
/// Carries no replicated state. An identical replay returns the identical DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommittedEffectDto {
    pub protocol_version: u32,
    /// Hex of the signed 16-byte persistent operation coordinate. The
    /// matching Observation attribution carries the same value.
    pub operation_id_hex: String,
    /// Base64 of the application effect bytes.
    pub effect_b64: String,
    /// Hex of the 32-byte Replica frontier root.
    pub frontier_root_hex: String,
    pub frontier_transaction_count: u64,
    /// Hex of the touched 16-byte Body ids.
    pub scope_body_ids_hex: Vec<String>,
    pub publication: WorldPublicationIdDto,
}

impl CommittedEffectDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)?;
        check_len(&self.operation_id_hex)?;
        check_hex16(&self.operation_id_hex)?;
        check_b64_payload(&self.effect_b64)?;
        check_len(&self.frontier_root_hex)?;
        check_hex32(&self.frontier_root_hex)?;
        for id in &self.scope_body_ids_hex {
            check_len(id)?;
            check_hex16(id)?;
        }
        self.publication.validate()
    }
}
json_codec!(CommittedEffectDto);

/// A projection DTO — derived local output, never replicated state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectionDto {
    pub protocol_version: u32,
    pub schema: String,
    pub schema_version: u32,
    /// Base64 of the projection bytes.
    pub bytes_b64: String,
    pub frontier_root_hex: String,
    pub frontier_transaction_count: u64,
    pub publication: WorldPublicationIdDto,
}

impl ProjectionDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)?;
        check_schema_id(&self.schema)?;
        check_b64_payload(&self.bytes_b64)?;
        check_len(&self.frontier_root_hex)?;
        check_hex32(&self.frontier_root_hex)?;
        self.publication.validate()
    }
}
json_codec!(ProjectionDto);

/// An observation cursor DTO. `sequence` is exclusive: replay delivers records
/// with a greater sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ObservationCursorDto {
    pub protocol_version: u32,
    pub epoch: u64,
    pub sequence: u64,
}

impl ObservationCursorDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)
    }
}
json_codec!(ObservationCursorDto);

/// One affected World and the exact immutable read publication installed for
/// it at this Observation's durable frontier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AffectedWorldPublicationDto {
    pub world: String,
    pub publication: WorldPublicationIdDto,
}

impl AffectedWorldPublicationDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_world(&self.world)?;
        self.publication.validate()
    }
}

/// One affected Body with its complete World namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AffectedBodyDto {
    pub world: String,
    pub body_id_hex: String,
}

impl AffectedBodyDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_world(&self.world)?;
        check_len(&self.body_id_hex)?;
        check_hex16(&self.body_id_hex)
    }
}

/// An observation DTO — a bounded Station-wide invalidation signal, carrying
/// no state. `reset: true` means the consumer must rebaseline and re-query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ObservationDto {
    pub protocol_version: u32,
    pub epoch: u64,
    pub sequence: u64,
    pub reset: bool,
    /// Canonically sorted full Body coordinates. Bare Body ids are ambiguous
    /// because one Station observation may span multiple Worlds.
    pub bodies: Vec<AffectedBodyDto>,
    pub frontier_root_hex: String,
    pub frontier_transaction_count: u64,
    /// Canonically sorted exact read coordinates for every affected World.
    /// Empty on reset and authority-only observations. Text offsets may be
    /// applied only to a projection carrying the matching coordinate.
    pub publications: Vec<AffectedWorldPublicationDto>,
}

impl ObservationDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)?;
        for body in &self.bodies {
            body.validate()?;
        }
        if self.bodies.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Invalid::BadIdentifier);
        }
        check_len(&self.frontier_root_hex)?;
        check_hex32(&self.frontier_root_hex)?;
        for publication in &self.publications {
            publication.validate()?;
        }
        if self.publications.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Invalid::BadIdentifier);
        }
        Ok(())
    }
}
json_codec!(ObservationDto);

/// The typed error DTO: a stable machine `code` (kebab-case) plus optional
/// human prose. Codes mirror the public error taxonomies; consumers match on
/// the code, never the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ErrorDto {
    pub protocol_version: u32,
    /// One of the stable error codes (e.g. `invalid-request`, `denied`,
    /// `unsupported-schema`, `authority-changed`, `request-id-conflict`,
    /// `station-dormant`, `limit-exceeded`, `persistence`, `reset-required`).
    pub code: String,
    pub message: String,
}

impl ErrorDto {
    fn validate(&self) -> Result<(), Invalid> {
        check_version(self.protocol_version)?;
        check_len(&self.code)?;
        if self.code.is_empty()
            || !self
                .code
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(Invalid::BadIdentifier);
        }
        check_len(&self.message)
    }

    /// The stable code for a public World error.
    pub fn code_for(error: &crate::world::Rejection) -> &'static str {
        use crate::world::Rejection as E;
        match error {
            E::InvalidRequest => "invalid-request",
            E::UnsupportedSchema => "unsupported-schema",
            E::UnsupportedSchemaVersion => "unsupported-schema-version",
            // One stable public code for every denial cause — consumers key
            // recovery UX off "denied"; the message carries the remedy.
            E::Denied(_) => "denied",
            E::NoActiveImplementation => "no-active-implementation",
            E::ImplementationUnavailable => "implementation-unavailable",
            E::Conflict => "conflict",
            E::LimitExceeded => "limit-exceeded",
            E::BodyRead(failure) => match failure {
                crate::world::BodyReadFailure::CapabilityUnavailable => {
                    "body-read-capability-unavailable"
                }
                crate::world::BodyReadFailure::Opaque(_) => "body-opaque",
                crate::world::BodyReadFailure::NotCollaborative(_) => "body-not-collaborative",
                crate::world::BodyReadFailure::SchemaAhead(_) => "body-schema-ahead",
                crate::world::BodyReadFailure::KeyUnavailable(_) => "body-key-unavailable",
                crate::world::BodyReadFailure::Corrupt(_) => "body-material-corrupt",
                crate::world::BodyReadFailure::Capacity(_) => "body-read-capacity",
                crate::world::BodyReadFailure::MaterialUnavailable(_) => {
                    "body-material-unavailable"
                }
                crate::world::BodyReadFailure::PublicationExpired(_) => "body-publication-expired",
                crate::world::BodyReadFailure::Interrupted(_) => "body-read-interrupted",
            },
            E::StateCorrupt => "world-state-corrupt",
            E::ContractViolation => "contract-violation",
        }
    }
}
json_codec!(ErrorDto);

/// Generate the committed JSON Schema 2020-12 bundle for the whole DTO
/// surface, deterministically (used by the drift gate).
pub fn schema_bundle() -> serde_json::Value {
    use schemars::generate::SchemaSettings;
    let settings = SchemaSettings::draft2020_12();
    let generator = settings.into_generator();
    let mut defs = serde_json::Map::new();
    macro_rules! add {
        ($ty:ty, $name:literal) => {
            let schema = generator.clone().into_root_schema_for::<$ty>();
            defs.insert(
                $name.to_string(),
                serde_json::to_value(schema).expect("schema json"),
            );
        };
    }
    add!(SubmitRequestDto, "SubmitRequestDto");
    add!(SignedSubmitDto, "SignedSubmitDto");
    add!(PublicationIdDto, "PublicationIdDto");
    add!(WorldPublicationIdDto, "WorldPublicationIdDto");
    add!(QueryRequestDto, "QueryRequestDto");
    add!(CommittedEffectDto, "CommittedEffectDto");
    add!(ProjectionDto, "ProjectionDto");
    add!(ObservationCursorDto, "ObservationCursorDto");
    add!(AffectedWorldPublicationDto, "AffectedWorldPublicationDto");
    add!(AffectedBodyDto, "AffectedBodyDto");
    add!(ObservationDto, "ObservationDto");
    add!(ErrorDto, "ErrorDto");
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://lait.dev/schema/dto/v1",
        "title": "LAIT external DTO surface v1",
        "$defs": defs,
        // Every identifier grammar the surface speaks, as language-neutral
        // string schemas. The Rust parsers are authoritative; the patterns
        // are drift-checked against them by `tests/dto_schema.rs`.
        "identifiers": identifier_schemas(),
    })
}

/// Language-neutral string schemas for every identifier grammar the external
/// surface carries. Each pattern mirrors the authoritative Rust parser.
pub fn identifier_schemas() -> serde_json::Value {
    serde_json::json!({
        "SpaceId": {
            "type": "string",
            "description": "Space id: ws_ + 26 base32 chars (case-insensitive)",
            "pattern": "^ws_[0-9A-Va-v]{26}$",
        },
        "ActorId": {
            "type": "string",
            "description": "Actor id: act_ + 64 lowercase hex chars (incept-event content address)",
            "pattern": "^act_[0-9a-f]{64}$",
        },
        "DeviceId": {
            "type": "string",
            "description": "Device id: 64 lowercase hex chars (an ed25519 public key)",
            "pattern": "^[0-9a-f]{64}$",
        },
        "StationId": {
            "type": "string",
            "description": "Station id in display form: 64 lowercase hex chars (the same key bytes as its DeviceId)",
            "pattern": "^[0-9a-f]{64}$",
        },
        "WorldId": {
            "type": "string",
            "description": "World id: reverse-domain, >=2 labels of [a-z0-9-] with no leading/trailing hyphen, 3-63 chars",
            "pattern": "^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$",
            "minLength": 3,
            "maxLength": 63,
        },
        "SchemaId": {
            "type": "string",
            "description": "Schema/encoding id: 1-63 lowercase ASCII, [a-z0-9][a-z0-9._-]*",
            "pattern": "^[a-z0-9][a-z0-9._-]{0,62}$",
        },
        "BodyIdHex": {
            "type": "string",
            "description": "Body id: 32 lowercase hex chars (16 canonical bytes)",
            "pattern": "^[0-9a-f]{32}$",
        },
        "RequestIdHex": {
            "type": "string",
            "description": "Request id: 32 lowercase hex chars (16-byte idempotency scope component)",
            "pattern": "^[0-9a-f]{32}$",
        },
        "FrontierRootHex": {
            "type": "string",
            "description": "Manifest frontier root: 64 lowercase hex chars (32 bytes)",
            "pattern": "^[0-9a-f]{64}$",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication() -> WorldPublicationIdDto {
        WorldPublicationIdDto {
            publication: PublicationIdDto {
                manifest_root_hex: "11".repeat(32),
                implementation_digest_hex: "22".repeat(32),
                extractor_schema_digest_hex: "33".repeat(32),
            },
            materialization: 1,
        }
    }

    fn sample() -> SubmitRequestDto {
        SubmitRequestDto {
            protocol_version: DTO_PROTOCOL_VERSION,
            world: "com.example.notes".into(),
            schema: "note".into(),
            schema_version: 1,
            request_id_hex: "0a".repeat(16),
            payload_b64: "aGVsbG8=".into(), // "hello"
        }
    }

    #[test]
    fn a_canonical_example_roundtrips_bidirectionally() {
        let dto = sample();
        let json = dto.to_json();
        let text = String::from_utf8(json.clone()).unwrap();
        assert!(text.contains("\"protocolVersion\":2"));
        assert!(text.contains("\"schemaVersion\":1"));
        assert_eq!(SubmitRequestDto::from_json(&json).unwrap(), dto);
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        let json = br#"{"protocolVersion":2,"world":"w.x","schema":"s","schemaVersion":1,"requestIdHex":"0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a","payloadB64":"","surprise":true}"#;
        assert!(matches!(
            SubmitRequestDto::from_json(json),
            Err(Invalid::Malformed)
        ));
    }

    #[test]
    fn a_missing_mandatory_field_is_rejected() {
        let json = br#"{"protocolVersion":2,"world":"w.x","schema":"s","payloadB64":""}"#;
        assert!(matches!(
            SubmitRequestDto::from_json(json),
            Err(Invalid::Malformed)
        ));
    }

    #[test]
    fn a_foreign_protocol_version_is_refused_not_negotiated() {
        let mut dto = sample();
        dto.protocol_version = 999;
        assert_eq!(
            SubmitRequestDto::from_json(&dto.to_json()),
            Err(Invalid::UnsupportedProtocol(999))
        );
    }

    #[test]
    fn over_long_strings_are_bounded() {
        let mut dto = sample();
        dto.world = "x".repeat(MAX_DTO_STRING + 1);
        assert_eq!(
            SubmitRequestDto::from_json(&dto.to_json()),
            Err(Invalid::StringTooLong)
        );
    }

    #[test]
    fn encodings_are_validated_by_decoded_length_not_text_length() {
        // Invalid base64 syntax.
        let mut dto = sample();
        dto.payload_b64 = "not base64!!".into();
        assert_eq!(
            SubmitRequestDto::from_json(&dto.to_json()),
            Err(Invalid::BadBase64)
        );
        // Valid base64 whose DECODED bytes exceed 1 MiB.
        let mut dto = sample();
        dto.payload_b64 = data_encoding::BASE64.encode(&vec![0u8; MAX_DTO_PAYLOAD + 1]);
        assert_eq!(
            SubmitRequestDto::from_json(&dto.to_json()),
            Err(Invalid::PayloadTooLarge)
        );
        // Hex with the wrong decoded length.
        let mut dto = sample();
        dto.request_id_hex = "0a".repeat(15);
        assert_eq!(
            SubmitRequestDto::from_json(&dto.to_json()),
            Err(Invalid::BadHex)
        );
        // A frontier root that is not 32 decoded bytes.
        let e = CommittedEffectDto {
            protocol_version: DTO_PROTOCOL_VERSION,
            operation_id_hex: "0c".repeat(16),
            effect_b64: "AA==".into(),
            frontier_root_hex: "ab".repeat(31),
            frontier_transaction_count: 3,
            scope_body_ids_hex: vec![],
            publication: publication(),
        };
        assert_eq!(
            CommittedEffectDto::from_json(&e.to_json()),
            Err(Invalid::BadHex)
        );
    }

    #[test]
    fn invalid_identifiers_are_rejected() {
        let mut dto = sample();
        dto.world = "NOT A WORLD".into();
        assert_eq!(
            SubmitRequestDto::from_json(&dto.to_json()),
            Err(Invalid::BadIdentifier)
        );
    }

    #[test]
    fn every_dto_roundtrips() {
        let q = QueryRequestDto {
            protocol_version: DTO_PROTOCOL_VERSION,
            world: "com.example.notes".into(),
            schema: "note".into(),
            schema_version: 1,
            payload_b64: "AA==".into(),
            publication: Some(publication().publication),
        };
        assert_eq!(QueryRequestDto::from_json(&q.to_json()).unwrap(), q);
        let p = ProjectionDto {
            protocol_version: DTO_PROTOCOL_VERSION,
            schema: "note".into(),
            schema_version: 1,
            bytes_b64: "AA==".into(),
            frontier_root_hex: "ab".repeat(32),
            frontier_transaction_count: 1,
            publication: publication(),
        };
        assert_eq!(ProjectionDto::from_json(&p.to_json()).unwrap(), p);
        let c = ObservationCursorDto {
            protocol_version: DTO_PROTOCOL_VERSION,
            epoch: 2,
            sequence: 7,
        };
        assert_eq!(ObservationCursorDto::from_json(&c.to_json()).unwrap(), c);
        let o = ObservationDto {
            protocol_version: DTO_PROTOCOL_VERSION,
            epoch: 1,
            sequence: 5,
            reset: true,
            bodies: vec![AffectedBodyDto {
                world: "com.example.notes".into(),
                body_id_hex: "0b".repeat(16),
            }],
            frontier_root_hex: "cd".repeat(32),
            frontier_transaction_count: 5,
            publications: vec![AffectedWorldPublicationDto {
                world: "com.example.notes".into(),
                publication: publication(),
            }],
        };
        assert_eq!(ObservationDto::from_json(&o.to_json()).unwrap(), o);
        let e = ErrorDto {
            protocol_version: DTO_PROTOCOL_VERSION,
            code: "request-id-conflict".into(),
            message: "the request id was reused with a different payload".into(),
        };
        assert_eq!(ErrorDto::from_json(&e.to_json()).unwrap(), e);
        // A malformed code is refused.
        let bad = ErrorDto {
            code: "Not A Code".into(),
            ..e
        };
        assert_eq!(
            ErrorDto::from_json(&bad.to_json()),
            Err(Invalid::BadIdentifier)
        );
    }
}
