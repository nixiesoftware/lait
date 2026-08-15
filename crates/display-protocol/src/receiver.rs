//! Closed receiver capability and health bodies.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::bounds::{
    MAX_ASSET_BYTES, MAX_BUILD_BYTES, MAX_FRAME_HEIGHT, MAX_FRAME_PIXELS, MAX_FRAME_WIDTH,
    MAX_LOCALE_BYTES, MAX_PROGRAM_ITEMS, MAX_STAGED_BYTES, MAX_STAGING_HORIZON_MS,
};
use crate::ids::{
    Challenge, DisplayAssetId, DisplayDeviceId, DisplayProgramItemId, ProgramRevision, Sha256Digest,
};
use crate::program::DisplayAssetMediaType;
use crate::{Refusal, PROTOCOL_MAJOR};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverPlatform {
    Desktop,
    Roku,
    AndroidTv,
    Webos,
    Tizen,
    Tvos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackTier {
    Frame,
    NativeHls,
    MseLive,
    NativeFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncClass {
    None,
    Boundary,
    PositionalB,
    PositionalA,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    Snapshot,
    Broadcast,
    NearRealtime,
    Realtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthGranularity {
    Coarse,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub scale_milli: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityCapabilities {
    pub native_screen_reader: bool,
    pub spoken_summary: bool,
    pub captions: bool,
    pub audio_description: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackCapabilities {
    pub tier: PlaybackTier,
    pub sync_class: SyncClass,
    pub rate_control_probed: bool,
    pub latency_class: LatencyClass,
    pub health_granularity: HealthGranularity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverCapabilities {
    pub protocol_major: u32,
    pub platform: ReceiverPlatform,
    pub build: String,
    pub viewport: Viewport,
    pub image_types: Vec<DisplayAssetMediaType>,
    pub max_asset_bytes: u32,
    pub max_staged_bytes: u32,
    pub max_program_items: u16,
    pub max_staging_horizon_ms: u32,
    pub locale: String,
    pub accessibility: AccessibilityCapabilities,
    pub playback: PlaybackCapabilities,
}

pub fn validate_capabilities(capabilities: &ReceiverCapabilities) -> Result<(), Refusal> {
    if capabilities.protocol_major != PROTOCOL_MAJOR {
        return Err(Refusal::Unsupported("protocol major"));
    }
    if capabilities.build.is_empty()
        || capabilities.build.len() > MAX_BUILD_BYTES
        || !capabilities.build.is_ascii()
        || capabilities
            .build
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(Refusal::BoundExceeded("receiver build"));
    }
    if capabilities.locale.is_empty()
        || capabilities.locale.len() > MAX_LOCALE_BYTES
        || !capabilities.locale.is_ascii()
    {
        return Err(Refusal::BoundExceeded("receiver locale"));
    }
    let viewport = &capabilities.viewport;
    if viewport.width == 0
        || viewport.width > MAX_FRAME_WIDTH
        || viewport.height == 0
        || viewport.height > MAX_FRAME_HEIGHT
        || !(500..=4_000).contains(&viewport.scale_milli)
    {
        return Err(Refusal::BoundExceeded("receiver viewport"));
    }
    let pixels = u64::from(viewport.width)
        .checked_mul(u64::from(viewport.height))
        .ok_or(Refusal::BoundExceeded("receiver viewport pixels"))?;
    if pixels > MAX_FRAME_PIXELS {
        return Err(Refusal::BoundExceeded("receiver viewport pixels"));
    }

    if capabilities.image_types.is_empty() || capabilities.image_types.len() > 3 {
        return Err(Refusal::BoundExceeded("receiver image types"));
    }
    if capabilities
        .image_types
        .iter()
        .any(|media_type| !media_type.is_image())
    {
        return Err(Refusal::InvalidShape("receiver image types"));
    }
    let unique: BTreeSet<_> = capabilities
        .image_types
        .iter()
        .map(|media_type| media_type.wire_name())
        .collect();
    if unique.len() != capabilities.image_types.len()
        || unique.iter().copied().ne(capabilities
            .image_types
            .iter()
            .map(|media_type| media_type.wire_name()))
    {
        return Err(Refusal::InvalidShape(
            "receiver image types must be sorted and unique",
        ));
    }
    if capabilities.max_asset_bytes == 0 || capabilities.max_asset_bytes > MAX_ASSET_BYTES {
        return Err(Refusal::BoundExceeded("receiver asset bytes"));
    }
    if capabilities.max_staged_bytes < capabilities.max_asset_bytes
        || capabilities.max_staged_bytes > MAX_STAGED_BYTES
    {
        return Err(Refusal::BoundExceeded("receiver staged bytes"));
    }
    if capabilities.max_program_items == 0
        || usize::from(capabilities.max_program_items) > MAX_PROGRAM_ITEMS
    {
        return Err(Refusal::BoundExceeded("receiver program items"));
    }
    if capabilities.max_staging_horizon_ms == 0
        || capabilities.max_staging_horizon_ms > MAX_STAGING_HORIZON_MS
    {
        return Err(Refusal::BoundExceeded("receiver staging horizon"));
    }

    let playback = &capabilities.playback;
    match playback.tier {
        PlaybackTier::Frame => {
            if playback.sync_class != SyncClass::Boundary
                || playback.rate_control_probed
                || playback.latency_class != LatencyClass::Snapshot
            {
                return Err(Refusal::InvalidShape("frame playback tier"));
            }
        }
        PlaybackTier::NativeHls => {
            if playback.sync_class != SyncClass::None
                || playback.rate_control_probed
                || playback.health_granularity != HealthGranularity::Coarse
            {
                return Err(Refusal::InvalidShape("native HLS tier"));
            }
        }
        PlaybackTier::MseLive => {
            if playback.sync_class != SyncClass::PositionalB {
                return Err(Refusal::InvalidShape("MSE live tier"));
            }
        }
        PlaybackTier::NativeFull => {}
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Online,
    Retrying,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    Ready,
    Staging,
    Displaying,
    Stale,
    Blank,
    Unsupported,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fault {
    None,
    Network,
    Authentication,
    RePairRequired,
    ProgramInvalid,
    AssetLength,
    AssetDigest,
    AssetDecode,
    Storage,
    UnsupportedProgram,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyBucket {
    Under16Ms,
    Under50Ms,
    Under100Ms,
    Under250Ms,
    Under1000Ms,
    Over1000Ms,
    Unobserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayedAsset {
    pub id: DisplayAssetId,
    pub sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverHealth {
    pub protocol_major: u32,
    pub platform: ReceiverPlatform,
    pub build: String,
    pub revision: ProgramRevision,
    pub current_item: DisplayProgramItemId,
    pub elapsed_ms: u32,
    pub last_displayed_asset: Option<DisplayedAsset>,
    pub connection: ConnectionState,
    pub playback: PlaybackState,
    pub last_error: Fault,
    pub staged_items: u16,
    pub staged_bytes: u32,
    pub decode_latency: LatencyBucket,
    pub swap_latency: LatencyBucket,
    pub drift_residual_ms: i32,
    pub correction_events: u32,
    pub pipeline_unobservable: bool,
}

pub fn validate_health(health: &ReceiverHealth) -> Result<(), Refusal> {
    if health.protocol_major != PROTOCOL_MAJOR {
        return Err(Refusal::Unsupported("protocol major"));
    }
    if health.build.is_empty()
        || health.build.len() > MAX_BUILD_BYTES
        || !health.build.is_ascii()
        || health.build.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(Refusal::BoundExceeded("receiver build"));
    }
    if usize::from(health.staged_items) > MAX_PROGRAM_ITEMS {
        return Err(Refusal::BoundExceeded("health staged item count"));
    }
    if health.staged_bytes > MAX_STAGED_BYTES {
        return Err(Refusal::BoundExceeded("health staged bytes"));
    }
    if !(-60_000..=60_000).contains(&health.drift_residual_ms) {
        return Err(Refusal::BoundExceeded("health drift residual"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeRequest {
    pub protocol_major: u32,
    pub device: DisplayDeviceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeResponse {
    pub protocol_major: u32,
    pub challenge: Challenge,
    pub expires_in_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiRefusalCode {
    InvalidRequest,
    AuthenticationFailed,
    ChallengeExpired,
    ChallengeConsumed,
    NotEnrolled,
    Unassigned,
    Revoked,
    RePairRequired,
    UnsupportedProtocol,
    BoundExceeded,
    TemporarilyUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRefusal {
    pub protocol_major: u32,
    pub code: ApiRefusalCode,
    pub retry_after_ms: Option<u32>,
    pub next_challenge: Option<Challenge>,
}
