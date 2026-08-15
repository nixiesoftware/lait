//! Challenge/HMAC request authentication and derived opaque identifiers.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::ids::{
    decode_hex_32, encode_hex, AuthenticationTag, Challenge, DisplayAssetId, DisplayAssignmentId,
    DisplayDeviceId, DisplayProgramId, DisplayProgramItemId, ProgramRevision, ProofKey,
    Sha256Digest,
};
use crate::program::DisplayAssetMediaType;
use crate::wire::Transcript;
use crate::{ProtocolError, PROTOCOL_MAJOR};

type HmacSha256 = Hmac<Sha256>;

pub const AUTHORIZATION_SCHEME: &str = "Astrolabe-HMAC";
pub const HEADER_PROTOCOL_MAJOR: &str = "X-Astrolabe-Protocol-Major";
pub const HEADER_ROUTE: &str = "X-Astrolabe-Route";
pub const HEADER_DEVICE: &str = "X-Astrolabe-Device";
pub const HEADER_ASSIGNMENT: &str = "X-Astrolabe-Assignment";
pub const HEADER_PROGRAM: &str = "X-Astrolabe-Program";
pub const HEADER_REVISION: &str = "X-Astrolabe-Revision";
pub const HEADER_CURRENT_ITEM: &str = "X-Astrolabe-Current-Item";
pub const HEADER_ELAPSED_MS: &str = "X-Astrolabe-Elapsed-Ms";
pub const HEADER_WAIT_MS: &str = "X-Astrolabe-Wait-Ms";
pub const HEADER_ASSET: &str = "X-Astrolabe-Asset";
pub const HEADER_RANGE_START: &str = "X-Astrolabe-Range-Start";
pub const HEADER_RANGE_LENGTH: &str = "X-Astrolabe-Range-Length";
pub const HEADER_CHALLENGE: &str = "X-Astrolabe-Challenge";
pub const HEADER_BODY_SHA256: &str = "X-Astrolabe-Body-SHA256";
pub const HEADER_NEXT_CHALLENGE: &str = "X-Astrolabe-Next-Challenge";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMethod {
    Get,
    Post,
}

impl RequestMethod {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestRoute {
    Capabilities,
    ProgramSnapshot,
    ProgramChanges,
    Asset,
    Health,
}

impl RequestRoute {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::ProgramSnapshot => "program_snapshot",
            Self::ProgramChanges => "program_changes",
            Self::Asset => "asset",
            Self::Health => "health",
        }
    }

    pub const fn path(self) -> &'static str {
        match self {
            Self::Capabilities => "/head/v1/capabilities",
            Self::ProgramSnapshot => "/head/v1/program",
            Self::ProgramChanges => "/head/v1/program/changes",
            Self::Asset => "/head/v1/assets/{opaque_asset}",
            Self::Health => "/head/v1/health",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetRange {
    pub start: u64,
    pub length: u32,
}

#[derive(Debug, Clone)]
pub struct RequestContext<'a> {
    pub protocol_major: u32,
    pub method: RequestMethod,
    pub route: RequestRoute,
    pub device: &'a DisplayDeviceId,
    pub assignment: Option<&'a DisplayAssignmentId>,
    pub program: Option<&'a DisplayProgramId>,
    pub revision: Option<&'a ProgramRevision>,
    pub current_item: Option<&'a DisplayProgramItemId>,
    pub elapsed_ms: Option<u32>,
    pub wait_ms: Option<u32>,
    pub asset: Option<&'a DisplayAssetId>,
    pub range: Option<AssetRange>,
    pub challenge: &'a Challenge,
    pub body_sha256: &'a Sha256Digest,
}

fn validate_context(context: &RequestContext<'_>) -> Result<(), ProtocolError> {
    if context.protocol_major != PROTOCOL_MAJOR {
        return Err(ProtocolError::Unsupported("protocol major"));
    }
    let valid = match context.route {
        RequestRoute::Capabilities => {
            context.method == RequestMethod::Post
                && context.current_item.is_none()
                && context.elapsed_ms.is_none()
                && context.wait_ms.is_none()
                && context.asset.is_none()
                && context.range.is_none()
        }
        RequestRoute::ProgramSnapshot => {
            context.method == RequestMethod::Get
                && context.revision.is_none()
                && context.current_item.is_none()
                && context.elapsed_ms.is_none()
                && context.wait_ms.is_none()
                && context.asset.is_none()
                && context.range.is_none()
        }
        RequestRoute::ProgramChanges => {
            context.method == RequestMethod::Get
                && context.assignment.is_some()
                && context.program.is_some()
                && context.revision.is_some()
                && context.current_item.is_some()
                && context.elapsed_ms.is_some()
                && context.wait_ms.is_some_and(|wait| wait > 0)
                && context.asset.is_none()
                && context.range.is_none()
        }
        RequestRoute::Asset => {
            context.method == RequestMethod::Get
                && context.assignment.is_some()
                && context.program.is_some()
                && context.revision.is_some()
                && context.current_item.is_none()
                && context.elapsed_ms.is_none()
                && context.wait_ms.is_none()
                && context.asset.is_some()
                && context.range.is_none_or(|range| range.length > 0)
        }
        RequestRoute::Health => {
            context.method == RequestMethod::Post
                && context.assignment.is_some()
                && context.program.is_some()
                && context.revision.is_some()
                && context.current_item.is_some()
                && context.elapsed_ms.is_some()
                && context.wait_ms.is_none()
                && context.asset.is_none()
                && context.range.is_none()
        }
    };
    if !valid {
        return Err(ProtocolError::InvalidShape(
            "request authentication context",
        ));
    }
    if context.assignment.is_some() != context.program.is_some() {
        return Err(ProtocolError::InvalidShape("assignment/program pair"));
    }
    Ok(())
}

pub fn request_transcript(context: &RequestContext<'_>) -> Result<Vec<u8>, ProtocolError> {
    validate_context(context)?;
    let mut transcript = Transcript::new(b"astrolabe-display/request/v1")?;
    transcript.u32(context.protocol_major)?;
    transcript.text(context.method.wire_name())?;
    transcript.text(context.route.wire_name())?;
    transcript.text(context.device.as_str())?;
    transcript.optional_text(context.assignment.map(DisplayAssignmentId::as_str))?;
    transcript.optional_text(context.program.map(DisplayProgramId::as_str))?;
    transcript.optional_text(context.revision.map(ProgramRevision::as_str))?;
    transcript.optional_text(context.current_item.map(DisplayProgramItemId::as_str))?;
    transcript.optional_u32(context.elapsed_ms)?;
    transcript.optional_u32(context.wait_ms)?;
    transcript.optional_text(context.asset.map(DisplayAssetId::as_str))?;
    transcript.optional_u64(context.range.map(|range| range.start))?;
    transcript.optional_u32(context.range.map(|range| range.length))?;
    transcript.text(context.challenge.as_str())?;
    transcript.text(context.body_sha256.as_str())?;
    Ok(transcript.finish())
}

pub fn sha256(bytes: &[u8]) -> Result<Sha256Digest, ProtocolError> {
    let digest = Sha256::digest(bytes);
    Sha256Digest::parse(encode_hex(&digest))
}

pub(crate) fn hmac_tag(
    key: &[u8; 32],
    transcript: &[u8],
) -> Result<AuthenticationTag, ProtocolError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| ProtocolError::InvalidEncoding("HMAC-SHA-256 key"))?;
    mac.update(transcript);
    AuthenticationTag::parse(encode_hex(&mac.finalize().into_bytes()))
}

pub fn authenticate_request(
    proof_key: &ProofKey,
    context: &RequestContext<'_>,
) -> Result<AuthenticationTag, ProtocolError> {
    let key = decode_hex_32(proof_key.as_str())?;
    let transcript = request_transcript(context)?;
    hmac_tag(&key, &transcript)
}

pub fn verify_request(
    proof_key: &ProofKey,
    context: &RequestContext<'_>,
    tag: &AuthenticationTag,
) -> Result<(), ProtocolError> {
    let key = decode_hex_32(proof_key.as_str())?;
    let expected = decode_hex_32(tag.as_str())?;
    let transcript = request_transcript(context)?;
    let mut mac = HmacSha256::new_from_slice(&key)
        .map_err(|_| ProtocolError::InvalidEncoding("HMAC-SHA-256 key"))?;
    mac.update(&transcript);
    mac.verify_slice(&expected)
        .map_err(|_| ProtocolError::Integrity("request authentication tag"))
}

fn identifier_hmac(
    identifier_key: &[u8; 32],
    domain: &'static [u8],
    fields: &[&[u8]],
) -> Result<String, ProtocolError> {
    let mut transcript = Transcript::new(domain)?;
    for field in fields {
        transcript.field(field)?;
    }
    let bytes = transcript.finish();
    Ok(hmac_tag(identifier_key, &bytes)?.to_string())
}

pub fn derive_program_item_id(
    identifier_key: &[u8; 32],
    assignment: &DisplayAssignmentId,
    package_item_id: &str,
) -> Result<DisplayProgramItemId, ProtocolError> {
    if package_item_id.is_empty() || package_item_id.len() > 128 {
        return Err(ProtocolError::BoundExceeded("package item id"));
    }
    let derived = identifier_hmac(
        identifier_key,
        b"astrolabe-display/program-item-id/v1",
        &[assignment.as_str().as_bytes(), package_item_id.as_bytes()],
    )?;
    DisplayProgramItemId::parse(derived)
}

pub fn derive_asset_id(
    identifier_key: &[u8; 32],
    assignment: &DisplayAssignmentId,
    media_type: DisplayAssetMediaType,
    encoded_len: u32,
    sha256: &Sha256Digest,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<DisplayAssetId, ProtocolError> {
    let encoded_len = encoded_len.to_be_bytes();
    let width = width.map(u32::to_be_bytes);
    let height = height.map(u32::to_be_bytes);
    let derived = identifier_hmac(
        identifier_key,
        b"astrolabe-display/asset-id/v1",
        &[
            assignment.as_str().as_bytes(),
            media_type.wire_name().as_bytes(),
            &encoded_len,
            sha256.as_str().as_bytes(),
            width.as_ref().map_or(&[], <[u8; 4]>::as_slice),
            height.as_ref().map_or(&[], <[u8; 4]>::as_slice),
        ],
    )?;
    DisplayAssetId::parse(derived)
}
