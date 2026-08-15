#![allow(
    clippy::arithmetic_side_effects,
    reason = "media arithmetic is checked against the protocol's explicit bounds"
)]
//! The native live-media wire carried by the Live plane's media lanes.
//!
//! The wire borrows moq-lite's useful shape, not its encoding: a Track is a
//! sequence of Groups, one Group occupies one QUIC unidirectional stream, and
//! each Group contains ordered length-delimited Frames. The concrete selectors,
//! canonical postcard bodies, bounds, and WebCodecs-shaped frame header below
//! belong to lait.
//!
//! A media stream is:
//!
//! ```text
//! 0x03 | framed GroupHeader | framed FrameHeader | raw payload | ... | FIN
//! ```
//!
//! Control and feedback are one canonical message per `0x04` flow. Media never
//! uses datagrams: a coded frame is larger than a path datagram and truncation
//! is not a recoverable representation of one.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use mechanics::station::Key;

use crate::plane::stream_kind;

/// This generation of the lait-owned media message vocabulary.
pub const PROTOCOL_VERSION: u16 = 1;
/// Default cap offered by a publisher: a late join never waits longer for a keyframe.
pub const DEFAULT_MAX_GROUP_DURATION_MS: u32 = 2_000;
/// Default end-to-end delivery budget advertised by a Live session.
pub const DEFAULT_MAX_LATENCY_MS: u32 = 3_000;
/// Hard protocol cap on one Group's presentation duration.
pub const MAX_GROUP_DURATION_MS: u32 = 10_000;
/// Hard protocol cap on a negotiated delivery-latency budget.
pub const MAX_LATENCY_MS: u32 = 30_000;
/// A Track name is an identifier, not a path or URL.
pub const MAX_TRACK_NAME_BYTES: usize = 128;
/// A codec string follows the WebCodecs codec-registry shape.
pub const MAX_CODEC_NAME_BYTES: usize = 64;
/// Codec extradata, bounded independently from a control message.
pub const MAX_DECODER_CONFIG_BYTES: usize = 64 * 1024;
/// One complete canonical `catalog.json` update. Catalogs are control-plane
/// metadata even though they travel as a Track, so they stay far below a
/// media-frame allocation.
pub const MAX_CATALOG_BYTES: usize = 256 * 1024;
/// A publisher cannot advertise an unbounded variant table.
pub const MAX_CATALOG_TRACKS: usize = 64;
/// Opaque coordinator HTTP rendition ids are identifiers, never URLs.
pub const MAX_RENDITION_ID_BYTES: usize = 128;
/// One Group header or Frame header.
pub const MAX_MEDIA_HEADER_BYTES: usize = 4 * 1024;
/// One encoded access unit. Checked before its payload is allocated.
pub const MAX_MEDIA_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// One Group cannot acquire an unbounded frame table.
pub const MAX_FRAMES_PER_GROUP: usize = 512;
/// Aggregate receiver memory ceiling for one materialized Group.
pub const MAX_MEDIA_GROUP_BYTES: usize = 32 * 1024 * 1024;
/// A connection may mint only this many subscription ids in either direction.
/// Ended ids remain spent, which bounds churn as well as concurrent work.
pub const MAX_SUBSCRIPTIONS_PER_SESSION: usize = 128;
/// At most the Live worker ceiling's worth of Groups may be incomplete.
pub const MAX_ACTIVE_GROUPS_PER_SESSION: usize = crate::plane::bounds::MAX_STREAM_WORKERS;
/// Bounded event fan-out from the plane to local consumers.
const EVENT_QUEUE: usize = 8;
/// Reset/stop code for a malformed or expired media flow.
pub const RESET_MEDIA: u32 = 3;
/// Well-known Track carrying full, canonical JSON catalog updates.
pub const CATALOG_TRACK: &str = "catalog.json";
/// TrackInfo codec marker for the non-media catalog Track.
pub const CATALOG_CODEC: &str = "json";
/// Catalog timestamps are expressed in milliseconds.
pub const CATALOG_TIMESCALE: u32 = 1_000;
/// This generation of the lait-owned catalog JSON contract.
pub const CATALOG_VERSION: u16 = 1;

mod control_kind {
    pub const SETUP: u8 = 0x01;
    pub const SUBSCRIBE: u8 = 0x02;
    pub const SUBSCRIBE_UPDATE: u8 = 0x03;
    pub const SUBSCRIBE_OK: u8 = 0x04;
    pub const SUBSCRIBE_DROP: u8 = 0x05;
    pub const SUBSCRIBE_END: u8 = 0x06;
    pub const FETCH: u8 = 0x07;
    pub const TRACK_INFO: u8 = 0x08;
    pub const REQUEST_KEYFRAME: u8 = 0x09;
    pub const CLOCK_PROBE: u8 = 0x0a;
    pub const CLOCK_REPLY: u8 = 0x0b;
    pub const PLAYOUT_TARGET: u8 = 0x0c;
    pub const GO_AWAY: u8 = 0x0d;
}

/// Why media bytes were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    TooLarge,
    NonCanonical,
    Bounds,
    Truncated,
    WrongLane(u8),
    UnknownControl(u8),
    FirstFrameNotKey,
    TimestampOrder,
    GroupDuration,
    PayloadLength,
    FeatureNotNegotiated,
    SetupRequired,
    Duplicate,
    UnknownSubscription,
    SubscriptionEnded,
    SubscriptionNotActive,
    TrackInfoRequired,
    WrongTrack,
    GoingAway,
    StaleGroup,
    UnsupportedCodec,
    BaselineRequired,
    CatalogRequired,
    TrackInfoMismatch,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Invalid {}

/// The application meaning of a Track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Catalog,
}

/// Whether an encoded chunk can be decoded independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Key,
    Delta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub protocol_version: u16,
    pub max_group_duration_ms: u32,
    pub max_latency_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    pub subscription_id: u64,
    pub track: String,
    pub priority: u8,
    /// `true` for VOD-like catch-up; live callers leave this false so a new
    /// Group can outrank an old one.
    pub ordered: bool,
    pub max_latency_ms: u32,
    pub start_group: Option<u64>,
    pub end_group: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeUpdate {
    pub subscription_id: u64,
    pub priority: u8,
    pub ordered: bool,
    pub max_latency_ms: u32,
    pub start_group: Option<u64>,
    pub end_group: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeOk {
    pub subscription_id: u64,
    pub publisher_priority: u8,
    pub publisher_max_latency_ms: u32,
    pub start_group: Option<u64>,
    pub end_group: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeDrop {
    pub subscription_id: u64,
    pub start_group: u64,
    pub end_group: u64,
    /// Application-specific and deliberately opaque to the media plane.
    pub error_code: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeEnd {
    pub subscription_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fetch {
    pub fetch_id: u64,
    pub track: String,
    pub group_sequence: u64,
    pub priority: u8,
}

/// Out-of-band decoder configuration for a Track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackInfo {
    pub track: String,
    pub kind: TrackKind,
    pub codec: String,
    pub timescale: u32,
    pub decoder_config: Vec<u8>,
    pub max_group_duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestKeyframe {
    pub track: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockProbe {
    pub probe_id: u64,
    pub t0_micros: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockReply {
    pub probe_id: u64,
    pub t0_micros: i64,
    pub t1_micros: i64,
    pub t2_micros: i64,
}

/// A correction target on the coordinator's shared timeline, never a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayoutTarget {
    pub track: String,
    pub shared_time_micros: i64,
    pub position_timestamp: i64,
    pub timescale: u32,
    pub playout_delay_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoAway {
    pub retry_after_ms: Option<u32>,
}

/// The thirteen control records. Together with GROUP and FRAME this is the
/// frozen fifteen-message vocabulary for protocol generation 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Setup(Setup),
    Subscribe(Subscribe),
    SubscribeUpdate(SubscribeUpdate),
    SubscribeOk(SubscribeOk),
    SubscribeDrop(SubscribeDrop),
    SubscribeEnd(SubscribeEnd),
    Fetch(Fetch),
    TrackInfo(TrackInfo),
    RequestKeyframe(RequestKeyframe),
    ClockProbe(ClockProbe),
    ClockReply(ClockReply),
    PlayoutTarget(PlayoutTarget),
    GoAway(GoAway),
}

impl Control {
    /// Explicit-selector encoding. Enum declaration order is not wire format.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let (kind, body) = match self {
            Self::Setup(value) => (control_kind::SETUP, encode_body(value)?),
            Self::Subscribe(value) => (control_kind::SUBSCRIBE, encode_body(value)?),
            Self::SubscribeUpdate(value) => (control_kind::SUBSCRIBE_UPDATE, encode_body(value)?),
            Self::SubscribeOk(value) => (control_kind::SUBSCRIBE_OK, encode_body(value)?),
            Self::SubscribeDrop(value) => (control_kind::SUBSCRIBE_DROP, encode_body(value)?),
            Self::SubscribeEnd(value) => (control_kind::SUBSCRIBE_END, encode_body(value)?),
            Self::Fetch(value) => (control_kind::FETCH, encode_body(value)?),
            Self::TrackInfo(value) => (control_kind::TRACK_INFO, encode_body(value)?),
            Self::RequestKeyframe(value) => (control_kind::REQUEST_KEYFRAME, encode_body(value)?),
            Self::ClockProbe(value) => (control_kind::CLOCK_PROBE, encode_body(value)?),
            Self::ClockReply(value) => (control_kind::CLOCK_REPLY, encode_body(value)?),
            Self::PlayoutTarget(value) => (control_kind::PLAYOUT_TARGET, encode_body(value)?),
            Self::GoAway(value) => (control_kind::GO_AWAY, encode_body(value)?),
        };
        let mut encoded = Vec::with_capacity(body.len().saturating_add(1));
        encoded.push(kind);
        encoded.extend_from_slice(&body);
        if encoded.len() > crate::plane::bounds::MAX_CONTROL_FRAME_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(encoded)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > crate::plane::bounds::MAX_CONTROL_FRAME_BYTES {
            return Err(Invalid::TooLarge);
        }
        let Some((&kind, body)) = bytes.split_first() else {
            return Err(Invalid::NonCanonical);
        };
        let decoded = match kind {
            control_kind::SETUP => Self::Setup(decode_body(body)?),
            control_kind::SUBSCRIBE => Self::Subscribe(decode_body(body)?),
            control_kind::SUBSCRIBE_UPDATE => Self::SubscribeUpdate(decode_body(body)?),
            control_kind::SUBSCRIBE_OK => Self::SubscribeOk(decode_body(body)?),
            control_kind::SUBSCRIBE_DROP => Self::SubscribeDrop(decode_body(body)?),
            control_kind::SUBSCRIBE_END => Self::SubscribeEnd(decode_body(body)?),
            control_kind::FETCH => Self::Fetch(decode_body(body)?),
            control_kind::TRACK_INFO => Self::TrackInfo(decode_body(body)?),
            control_kind::REQUEST_KEYFRAME => Self::RequestKeyframe(decode_body(body)?),
            control_kind::CLOCK_PROBE => Self::ClockProbe(decode_body(body)?),
            control_kind::CLOCK_REPLY => Self::ClockReply(decode_body(body)?),
            control_kind::PLAYOUT_TARGET => Self::PlayoutTarget(decode_body(body)?),
            control_kind::GO_AWAY => Self::GoAway(decode_body(body)?),
            other => return Err(Invalid::UnknownControl(other)),
        };
        decoded.validate()?;
        if decoded.encode()?.as_slice() != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(decoded)
    }

    fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Setup(value) => {
                if value.protocol_version != PROTOCOL_VERSION {
                    return Err(Invalid::Bounds);
                }
                validate_durations(value.max_group_duration_ms, value.max_latency_ms)
            }
            Self::Subscribe(value) => {
                validate_track(&value.track)?;
                validate_latency(value.max_latency_ms)?;
                validate_range(value.start_group, value.end_group)
            }
            Self::SubscribeUpdate(value) => {
                validate_latency(value.max_latency_ms)?;
                validate_range(value.start_group, value.end_group)
            }
            Self::SubscribeOk(value) => {
                validate_latency(value.publisher_max_latency_ms)?;
                validate_range(value.start_group, value.end_group)
            }
            Self::SubscribeDrop(value) => {
                if value.start_group > value.end_group {
                    return Err(Invalid::Bounds);
                }
                Ok(())
            }
            Self::SubscribeEnd(_) | Self::ClockProbe(_) | Self::ClockReply(_) => Ok(()),
            Self::Fetch(value) => validate_track(&value.track),
            Self::TrackInfo(value) => {
                validate_track(&value.track)?;
                if value.timescale == 0
                    || value.decoder_config.len() > MAX_DECODER_CONFIG_BYTES
                    || value.max_group_duration_ms == 0
                    || value.max_group_duration_ms > MAX_GROUP_DURATION_MS
                {
                    return Err(Invalid::Bounds);
                }
                match value.kind {
                    TrackKind::Catalog => {
                        if value.track != CATALOG_TRACK
                            || value.codec != CATALOG_CODEC
                            || value.timescale != CATALOG_TIMESCALE
                            || !value.decoder_config.is_empty()
                        {
                            return Err(Invalid::Bounds);
                        }
                    }
                    TrackKind::Video | TrackKind::Audio => {
                        let codec = codec_family(value.kind, &value.codec)?;
                        if value.track == CATALOG_TRACK
                            || (codec != CodecFamily::Av1 && value.decoder_config.is_empty())
                        {
                            return Err(Invalid::Bounds);
                        }
                    }
                }
                Ok(())
            }
            Self::RequestKeyframe(value) => validate_track(&value.track),
            Self::PlayoutTarget(value) => {
                validate_track(&value.track)?;
                if value.timescale == 0 || value.playout_delay_ms > MAX_LATENCY_MS {
                    return Err(Invalid::Bounds);
                }
                Ok(())
            }
            Self::GoAway(value) => {
                if value
                    .retry_after_ms
                    .is_some_and(|delay| delay > MAX_LATENCY_MS)
                {
                    return Err(Invalid::Bounds);
                }
                Ok(())
            }
        }
    }
}

/// The first record on each media Group stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupHeader {
    pub subscription_id: u64,
    pub track: String,
    pub track_kind: TrackKind,
    pub group_sequence: u64,
    /// Coordinator/shared-timeline time when the Group became available.
    pub published_at_micros: i64,
    pub timescale: u32,
    pub max_group_duration_ms: u32,
}

impl GroupHeader {
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes = encode_body(self)?;
        if bytes.len() > MAX_MEDIA_HEADER_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_MEDIA_HEADER_BYTES {
            return Err(Invalid::TooLarge);
        }
        let decoded: Self = decode_body(bytes)?;
        decoded.validate()?;
        if decoded.encode()?.as_slice() != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(decoded)
    }

    fn validate(&self) -> Result<(), Invalid> {
        validate_track(&self.track)?;
        if self.timescale == 0
            || self.max_group_duration_ms == 0
            || self.max_group_duration_ms > MAX_GROUP_DURATION_MS
        {
            return Err(Invalid::Bounds);
        }
        if (self.track_kind == TrackKind::Catalog
            && (self.track != CATALOG_TRACK || self.timescale != CATALOG_TIMESCALE))
            || (self.track_kind != TrackKind::Catalog && self.track == CATALOG_TRACK)
        {
            return Err(Invalid::Bounds);
        }
        Ok(())
    }
}

/// WebCodecs-shaped metadata preceding one raw encoded access unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameHeader {
    pub timestamp: i64,
    pub duration: Option<u64>,
    pub timescale: u32,
    pub kind: FrameKind,
    pub payload_len: u32,
}

impl FrameHeader {
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes = encode_body(self)?;
        if bytes.len() > MAX_MEDIA_HEADER_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_MEDIA_HEADER_BYTES {
            return Err(Invalid::TooLarge);
        }
        let decoded: Self = decode_body(bytes)?;
        decoded.validate()?;
        if decoded.encode()?.as_slice() != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(decoded)
    }

    fn validate(&self) -> Result<(), Invalid> {
        let len = usize::try_from(self.payload_len).map_err(|_| Invalid::Bounds)?;
        if self.timescale == 0 || len > MAX_MEDIA_FRAME_BYTES {
            return Err(Invalid::Bounds);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedGroup {
    pub header: GroupHeader,
    pub frames: Vec<Frame>,
}

/// One full independent `catalog.json` update.
///
/// lait deliberately does not implement JSON Patch/delta catalogs: every
/// update is one canonical document in one Group, so a late join never needs
/// state that may already have expired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub version: u16,
    pub jitter_hint_ms: u32,
    pub tracks: Vec<CatalogTrack>,
}

/// Decoder and coordinator-edge information for one raw peer Track.
///
/// `decoder_config_hex` is exactly the WebCodecs decoder-description bytes.
/// HTTP rendition ids are opaque assignment-bound names resolved by the
/// coordinator; paths and URLs are never admitted to this protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogTrack {
    pub track: String,
    pub kind: TrackKind,
    pub codec: String,
    pub timescale: u32,
    pub decoder_config_hex: String,
    pub max_group_duration_ms: u32,
    pub target_latency_ms: u32,
    pub bitrate_bps: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate_milli: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmaf_rendition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hls_v3_rendition: Option<String>,
}

/// Generation-one decoder capability. H.264 and AAC are the mandatory
/// baseline; AV1 is the only optional native codec in this generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecoderSupport {
    av1: bool,
}

impl DecoderSupport {
    pub const fn baseline() -> Self {
        Self { av1: false }
    }

    pub const fn with_av1(mut self) -> Self {
        self.av1 = true;
        self
    }

    pub fn supports(self, track: &CatalogTrack) -> bool {
        match codec_family(track.kind, &track.codec) {
            Ok(CodecFamily::H264 | CodecFamily::Aac) => true,
            Ok(CodecFamily::Av1) => self.av1,
            Err(_) => false,
        }
    }
}

impl Catalog {
    pub fn encode_canonical(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| Invalid::NonCanonical)?;
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(Invalid::TooLarge);
        }
        let catalog: Self = serde_json::from_slice(bytes).map_err(|_| Invalid::NonCanonical)?;
        catalog.validate()?;
        if catalog.encode_canonical()?.as_slice() != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(catalog)
    }

    /// Decode an atomic catalog update from its one-Frame Group.
    pub fn from_group(group: &ReceivedGroup) -> Result<Self, Invalid> {
        if group.header.track != CATALOG_TRACK
            || group.header.track_kind != TrackKind::Catalog
            || group.header.timescale != CATALOG_TIMESCALE
            || group.frames.len() != 1
        {
            return Err(Invalid::Bounds);
        }
        let frame = group.frames.first().ok_or(Invalid::Bounds)?;
        if frame.header.kind != FrameKind::Key
            || frame.header.duration.is_some()
            || frame.header.timescale != CATALOG_TIMESCALE
            || usize::try_from(frame.header.payload_len).ok() != Some(frame.payload.len())
        {
            return Err(Invalid::Bounds);
        }
        Self::decode_canonical(&frame.payload)
    }

    pub fn track(&self, name: &str) -> Option<&CatalogTrack> {
        self.tracks.iter().find(|track| track.track == name)
    }

    fn validate(&self) -> Result<(), Invalid> {
        if self.version != CATALOG_VERSION
            || self.tracks.is_empty()
            || self.tracks.len() > MAX_CATALOG_TRACKS
            || self.jitter_hint_ms > MAX_LATENCY_MS
        {
            return Err(Invalid::Bounds);
        }
        let mut names = std::collections::BTreeSet::new();
        let mut has_video = false;
        let mut has_h264 = false;
        let mut has_audio = false;
        let mut has_aac = false;
        for track in &self.tracks {
            track.validate()?;
            if !names.insert(track.track.as_str()) {
                return Err(Invalid::Duplicate);
            }
            if self.jitter_hint_ms > track.target_latency_ms {
                return Err(Invalid::Bounds);
            }
            match codec_family(track.kind, &track.codec)? {
                CodecFamily::H264 => {
                    has_video = true;
                    has_h264 = true;
                }
                CodecFamily::Av1 => has_video = true,
                CodecFamily::Aac => {
                    has_audio = true;
                    has_aac = true;
                }
            }
        }
        if (has_video && !has_h264) || (has_audio && !has_aac) {
            return Err(Invalid::BaselineRequired);
        }
        Ok(())
    }
}

impl CatalogTrack {
    pub fn decoder_config(&self) -> Result<Vec<u8>, Invalid> {
        data_encoding::HEXLOWER
            .decode(self.decoder_config_hex.as_bytes())
            .map_err(|_| Invalid::Bounds)
    }

    pub fn track_info(&self) -> Result<TrackInfo, Invalid> {
        self.validate()?;
        Ok(TrackInfo {
            track: self.track.clone(),
            kind: self.kind,
            codec: self.codec.clone(),
            timescale: self.timescale,
            decoder_config: self.decoder_config()?,
            max_group_duration_ms: self.max_group_duration_ms,
        })
    }

    fn validate(&self) -> Result<(), Invalid> {
        validate_track(&self.track)?;
        if self.track == CATALOG_TRACK
            || self.kind == TrackKind::Catalog
            || self.timescale == 0
            || self.decoder_config_hex.len() > MAX_DECODER_CONFIG_BYTES.saturating_mul(2)
            || self.max_group_duration_ms == 0
            || self.max_group_duration_ms > MAX_GROUP_DURATION_MS
            || self.bitrate_bps == 0
            || self.bitrate_bps > 1_000_000_000
        {
            return Err(Invalid::Bounds);
        }
        validate_latency(self.target_latency_ms)?;
        let codec = codec_family(self.kind, &self.codec)?;
        let decoder_config = self.decoder_config()?;
        if decoder_config.len() > MAX_DECODER_CONFIG_BYTES
            || (codec != CodecFamily::Av1 && decoder_config.is_empty())
        {
            return Err(Invalid::Bounds);
        }
        match self.kind {
            TrackKind::Video => {
                if !matches!(
                    (self.width, self.height),
                    (Some(1..=16_384), Some(1..=16_384))
                ) || !matches!(self.frame_rate_milli, Some(1..=240_000))
                    || self.sample_rate.is_some()
                    || self.channels.is_some()
                {
                    return Err(Invalid::Bounds);
                }
            }
            TrackKind::Audio => {
                if self.width.is_some()
                    || self.height.is_some()
                    || self.frame_rate_milli.is_some()
                    || !matches!(self.sample_rate, Some(8_000..=384_000))
                    || !matches!(self.channels, Some(1..=32))
                {
                    return Err(Invalid::Bounds);
                }
            }
            TrackKind::Catalog => return Err(Invalid::Bounds),
        }
        if let Some(group) = &self.render_group {
            validate_opaque_id(group, 64)?;
        }
        if let Some(rendition) = &self.cmaf_rendition {
            validate_opaque_id(rendition, MAX_RENDITION_ID_BYTES)?;
        }
        if let Some(rendition) = &self.hls_v3_rendition {
            validate_opaque_id(rendition, MAX_RENDITION_ID_BYTES)?;
            if codec == CodecFamily::Av1 {
                return Err(Invalid::UnsupportedCodec);
            }
        }
        Ok(())
    }
}

impl TrackInfo {
    pub fn matches_catalog(&self, track: &CatalogTrack) -> Result<(), Invalid> {
        if self == &track.track_info()? {
            Ok(())
        } else {
            Err(Invalid::TrackInfoMismatch)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodecFamily {
    H264,
    Aac,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Sent,
    Received,
}

impl Direction {
    fn opposite(self) -> Self {
        match self {
            Self::Sent => Self::Received,
            Self::Received => Self::Sent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscriptionPhase {
    Pending,
    Active,
    Ended,
}

enum GroupReadiness {
    Pending(Duration),
    Ready,
}

enum GroupAdmission {
    Pending(Duration),
    Ready(IncomingGroup),
}

#[derive(Debug, Clone)]
struct SubscriptionRecord {
    request: Subscribe,
    phase: SubscriptionPhase,
}

struct ActiveGroup {
    stamp: GroupStamp,
    budget: Duration,
    expiry: tokio::sync::watch::Sender<Option<Instant>>,
}

#[derive(Debug, Clone)]
struct CatalogSnapshot {
    sequence: u64,
    catalog: Catalog,
}

#[derive(Default)]
struct SessionState {
    sent_setup: Option<Setup>,
    received_setup: Option<Setup>,
    sent_go_away: bool,
    received_go_away: bool,
    /// Subscriptions this Station initiated and therefore receives Groups for.
    outbound: std::collections::BTreeMap<u64, SubscriptionRecord>,
    /// Subscriptions the peer initiated and this Station publishes Groups for.
    inbound: std::collections::BTreeMap<u64, SubscriptionRecord>,
    sent_tracks: std::collections::BTreeMap<String, TrackInfo>,
    received_tracks: std::collections::BTreeMap<String, TrackInfo>,
    sent_catalog: Option<CatalogSnapshot>,
    received_catalog: Option<CatalogSnapshot>,
    active_groups: std::collections::BTreeMap<(u64, u64), ActiveGroup>,
    /// Highest sequence observed even after its stream completes. Without this,
    /// an older stream arriving just after the newer one finished would become
    /// "newest" again and recover a full latency budget.
    newest_groups: std::collections::BTreeMap<u64, GroupStamp>,
}

impl SessionState {
    fn record(&mut self, direction: Direction, control: &Control) -> Result<(), Invalid> {
        if let Control::Setup(setup) = control {
            let slot = match direction {
                Direction::Sent => &mut self.sent_setup,
                Direction::Received => &mut self.received_setup,
            };
            if slot.is_some() {
                return Err(Invalid::Duplicate);
            }
            *slot = Some(setup.clone());
            return Ok(());
        }
        self.require_setup(direction)?;
        self.require_setup(direction.opposite())?;

        match control {
            Control::Setup(_) => Err(Invalid::Duplicate),
            Control::Subscribe(request) => {
                if self.going_away(direction.opposite()) {
                    return Err(Invalid::GoingAway);
                }
                if request.max_latency_ms > self.negotiated_latency_ms()? {
                    return Err(Invalid::Bounds);
                }
                let subscriptions = self.owner_mut(direction);
                if subscriptions.contains_key(&request.subscription_id) {
                    return Err(Invalid::Duplicate);
                }
                if subscriptions.len() >= MAX_SUBSCRIPTIONS_PER_SESSION {
                    return Err(Invalid::TooLarge);
                }
                subscriptions.insert(
                    request.subscription_id,
                    SubscriptionRecord {
                        request: request.clone(),
                        phase: SubscriptionPhase::Pending,
                    },
                );
                Ok(())
            }
            Control::SubscribeUpdate(update) => {
                if update.max_latency_ms > self.negotiated_latency_ms()? {
                    return Err(Invalid::Bounds);
                }
                let record = self
                    .owner_mut(direction)
                    .get_mut(&update.subscription_id)
                    .ok_or(Invalid::UnknownSubscription)?;
                if record.phase == SubscriptionPhase::Ended {
                    return Err(Invalid::SubscriptionEnded);
                }
                record.request.priority = update.priority;
                record.request.ordered = update.ordered;
                record.request.max_latency_ms = update.max_latency_ms;
                record.request.start_group = update.start_group;
                record.request.end_group = update.end_group;
                if direction == Direction::Sent {
                    let tightened = Duration::from_millis(u64::from(update.max_latency_ms));
                    for ((subscription, _), active) in &mut self.active_groups {
                        if *subscription != update.subscription_id || tightened >= active.budget {
                            continue;
                        }
                        active.budget = tightened;
                        let current = *active.expiry.borrow();
                        if let Some(current) = current {
                            let candidate = active
                                .stamp
                                .observed_at
                                .checked_add(tightened)
                                .unwrap_or(current);
                            if candidate < current {
                                active.expiry.send_replace(Some(candidate));
                            }
                        }
                    }
                }
                Ok(())
            }
            Control::SubscribeOk(ok) => {
                let negotiated_latency = self.negotiated_latency_ms()?;
                let record = self
                    .responder_mut(direction)
                    .get_mut(&ok.subscription_id)
                    .ok_or(Invalid::UnknownSubscription)?;
                if ok.publisher_max_latency_ms > negotiated_latency
                    || ok.publisher_max_latency_ms > record.request.max_latency_ms
                {
                    return Err(Invalid::Bounds);
                }
                if ok.start_group.is_some_and(|start| {
                    record
                        .request
                        .start_group
                        .is_some_and(|requested| start < requested)
                }) || ok.end_group.is_some_and(|end| {
                    record
                        .request
                        .end_group
                        .is_some_and(|requested| end > requested)
                }) {
                    return Err(Invalid::Bounds);
                }
                match record.phase {
                    SubscriptionPhase::Pending => {
                        record.request.max_latency_ms = ok.publisher_max_latency_ms;
                        if ok.start_group.is_some() {
                            record.request.start_group = ok.start_group;
                        }
                        if ok.end_group.is_some() {
                            record.request.end_group = ok.end_group;
                        }
                        record.phase = SubscriptionPhase::Active;
                        Ok(())
                    }
                    SubscriptionPhase::Active => Err(Invalid::Duplicate),
                    SubscriptionPhase::Ended => Err(Invalid::SubscriptionEnded),
                }
            }
            Control::SubscribeDrop(drop) => {
                let record = self
                    .responder(direction)
                    .get(&drop.subscription_id)
                    .ok_or(Invalid::UnknownSubscription)?;
                if record.phase == SubscriptionPhase::Ended {
                    return Err(Invalid::SubscriptionEnded);
                }
                if record
                    .request
                    .start_group
                    .is_some_and(|first| drop.start_group < first)
                    || record
                        .request
                        .end_group
                        .is_some_and(|last| drop.end_group > last)
                {
                    return Err(Invalid::Bounds);
                }
                Ok(())
            }
            Control::SubscribeEnd(end) => {
                let owner = direction.opposite();
                let record = self
                    .owner_mut(owner)
                    .get_mut(&end.subscription_id)
                    .ok_or(Invalid::UnknownSubscription)?;
                if record.phase == SubscriptionPhase::Ended {
                    return Err(Invalid::SubscriptionEnded);
                }
                record.phase = SubscriptionPhase::Ended;
                if owner == Direction::Sent {
                    let now = Instant::now();
                    for ((subscription, _), active) in &self.active_groups {
                        if *subscription == end.subscription_id {
                            active.expiry.send_replace(Some(now));
                        }
                    }
                }
                Ok(())
            }
            Control::TrackInfo(info) => {
                if info.max_group_duration_ms > self.negotiated_group_duration_ms()? {
                    return Err(Invalid::Bounds);
                }
                self.require_active_track(direction.opposite(), &info.track)?;
                if info.kind != TrackKind::Catalog {
                    let advertised = self
                        .catalog(direction)
                        .and_then(|catalog| catalog.catalog.track(&info.track))
                        .ok_or(Invalid::CatalogRequired)?;
                    info.matches_catalog(advertised)?;
                }
                match direction {
                    Direction::Sent => &mut self.sent_tracks,
                    Direction::Received => &mut self.received_tracks,
                }
                .insert(info.track.clone(), info.clone());
                Ok(())
            }
            Control::RequestKeyframe(request) => {
                self.require_active_track(direction, &request.track)
            }
            Control::PlayoutTarget(target) => {
                if target.playout_delay_ms > self.negotiated_latency_ms()? {
                    return Err(Invalid::Bounds);
                }
                self.require_active_track(direction.opposite(), &target.track)
            }
            Control::GoAway(_) => {
                let going_away = match direction {
                    Direction::Sent => &mut self.sent_go_away,
                    Direction::Received => &mut self.received_go_away,
                };
                if *going_away {
                    return Err(Invalid::Duplicate);
                }
                *going_away = true;
                Ok(())
            }
            Control::Fetch(_) | Control::ClockProbe(_) | Control::ClockReply(_) => Ok(()),
        }
    }

    fn require_setup(&self, direction: Direction) -> Result<(), Invalid> {
        let setup = match direction {
            Direction::Sent => &self.sent_setup,
            Direction::Received => &self.received_setup,
        };
        setup.as_ref().map(|_| ()).ok_or(Invalid::SetupRequired)
    }

    fn negotiated_latency_ms(&self) -> Result<u32, Invalid> {
        Ok(self
            .sent_setup
            .as_ref()
            .ok_or(Invalid::SetupRequired)?
            .max_latency_ms
            .min(
                self.received_setup
                    .as_ref()
                    .ok_or(Invalid::SetupRequired)?
                    .max_latency_ms,
            ))
    }

    fn negotiated_group_duration_ms(&self) -> Result<u32, Invalid> {
        Ok(self
            .sent_setup
            .as_ref()
            .ok_or(Invalid::SetupRequired)?
            .max_group_duration_ms
            .min(
                self.received_setup
                    .as_ref()
                    .ok_or(Invalid::SetupRequired)?
                    .max_group_duration_ms,
            ))
    }

    fn going_away(&self, direction: Direction) -> bool {
        match direction {
            Direction::Sent => self.sent_go_away,
            Direction::Received => self.received_go_away,
        }
    }

    fn catalog(&self, direction: Direction) -> Option<&CatalogSnapshot> {
        match direction {
            Direction::Sent => self.sent_catalog.as_ref(),
            Direction::Received => self.received_catalog.as_ref(),
        }
    }

    fn record_catalog(
        &mut self,
        direction: Direction,
        group: &ReceivedGroup,
    ) -> Result<(), Invalid> {
        self.group_budget(direction.opposite(), &group.header)?;
        let catalog = Catalog::from_group(group)?;
        self.record_catalog_value(direction, group.header.group_sequence, catalog)
    }

    fn record_catalog_value(
        &mut self,
        direction: Direction,
        sequence: u64,
        catalog: Catalog,
    ) -> Result<(), Invalid> {
        catalog.validate()?;
        let slot = match direction {
            Direction::Sent => &mut self.sent_catalog,
            Direction::Received => &mut self.received_catalog,
        };
        if slot
            .as_ref()
            .is_some_and(|current| current.sequence >= sequence)
        {
            return Err(Invalid::Duplicate);
        }
        *slot = Some(CatalogSnapshot { sequence, catalog });
        Ok(())
    }

    fn owner(&self, direction: Direction) -> &std::collections::BTreeMap<u64, SubscriptionRecord> {
        match direction {
            Direction::Sent => &self.outbound,
            Direction::Received => &self.inbound,
        }
    }

    fn owner_mut(
        &mut self,
        direction: Direction,
    ) -> &mut std::collections::BTreeMap<u64, SubscriptionRecord> {
        match direction {
            Direction::Sent => &mut self.outbound,
            Direction::Received => &mut self.inbound,
        }
    }

    fn responder(
        &self,
        direction: Direction,
    ) -> &std::collections::BTreeMap<u64, SubscriptionRecord> {
        self.owner(direction.opposite())
    }

    fn responder_mut(
        &mut self,
        direction: Direction,
    ) -> &mut std::collections::BTreeMap<u64, SubscriptionRecord> {
        self.owner_mut(direction.opposite())
    }

    fn require_active_track(&self, owner: Direction, track: &str) -> Result<(), Invalid> {
        self.owner(owner)
            .values()
            .any(|record| {
                record.phase == SubscriptionPhase::Active && record.request.track == track
            })
            .then_some(())
            .ok_or(Invalid::SubscriptionNotActive)
    }

    fn group_budget(&self, owner: Direction, header: &GroupHeader) -> Result<Duration, Invalid> {
        let record = self
            .owner(owner)
            .get(&header.subscription_id)
            .ok_or(Invalid::UnknownSubscription)?;
        if record.phase != SubscriptionPhase::Active {
            return Err(Invalid::SubscriptionNotActive);
        }
        if record.request.track != header.track {
            return Err(Invalid::WrongTrack);
        }
        let (tracks, catalog) = match owner {
            Direction::Sent => (&self.received_tracks, self.received_catalog.as_ref()),
            Direction::Received => (&self.sent_tracks, self.sent_catalog.as_ref()),
        };
        let track = tracks
            .get(&header.track)
            .ok_or(Invalid::TrackInfoRequired)?;
        if header.track_kind != TrackKind::Catalog {
            let advertised = catalog
                .and_then(|catalog| catalog.catalog.track(&header.track))
                .ok_or(Invalid::CatalogRequired)?;
            track.matches_catalog(advertised)?;
        }
        if track.kind != header.track_kind {
            return Err(Invalid::Bounds);
        }
        if track.timescale != header.timescale {
            return Err(Invalid::Bounds);
        }
        if record
            .request
            .start_group
            .is_some_and(|first| header.group_sequence < first)
            || record
                .request
                .end_group
                .is_some_and(|last| header.group_sequence > last)
        {
            return Err(Invalid::Bounds);
        }
        if header.max_group_duration_ms > self.negotiated_group_duration_ms()? {
            return Err(Invalid::Bounds);
        }
        Ok(Duration::from_millis(u64::from(
            record
                .request
                .max_latency_ms
                .min(self.negotiated_latency_ms()?),
        )))
    }

    fn received_group_readiness(&self, header: &GroupHeader) -> Result<GroupReadiness, Invalid> {
        let record = self
            .outbound
            .get(&header.subscription_id)
            .ok_or(Invalid::UnknownSubscription)?;
        if record.phase == SubscriptionPhase::Ended {
            return Err(Invalid::SubscriptionEnded);
        }
        if record.request.track != header.track {
            return Err(Invalid::WrongTrack);
        }
        if record
            .request
            .start_group
            .is_some_and(|first| header.group_sequence < first)
            || record
                .request
                .end_group
                .is_some_and(|last| header.group_sequence > last)
            || header.max_group_duration_ms > self.negotiated_group_duration_ms()?
        {
            return Err(Invalid::Bounds);
        }
        let budget = Duration::from_millis(u64::from(
            record
                .request
                .max_latency_ms
                .min(self.negotiated_latency_ms()?),
        ));
        if record.phase == SubscriptionPhase::Pending {
            return Ok(GroupReadiness::Pending(budget));
        }
        let Some(track) = self.received_tracks.get(&header.track) else {
            return Ok(GroupReadiness::Pending(budget));
        };
        if track.kind != header.track_kind {
            return Err(Invalid::Bounds);
        }
        if track.timescale != header.timescale {
            return Err(Invalid::Bounds);
        }
        Ok(GroupReadiness::Ready)
    }

    fn begin_received_group(
        &mut self,
        state: Arc<std::sync::Mutex<Self>>,
        header: &GroupHeader,
    ) -> Result<IncomingGroup, Invalid> {
        let budget = self.group_budget(Direction::Sent, header)?;
        let key = (header.subscription_id, header.group_sequence);
        if self.active_groups.contains_key(&key) {
            return Err(Invalid::Duplicate);
        }
        if self.active_groups.len() >= MAX_ACTIVE_GROUPS_PER_SESSION {
            return Err(Invalid::TooLarge);
        }

        let now = Instant::now();
        let stamp = GroupStamp {
            sequence: header.group_sequence,
            published_at_micros: header.published_at_micros,
            observed_at: now,
        };
        if self
            .newest_groups
            .get(&header.subscription_id)
            .is_some_and(|newest| newest.sequence == stamp.sequence)
        {
            return Err(Invalid::Duplicate);
        }
        let newest = match self.newest_groups.get(&header.subscription_id).copied() {
            Some(current) if current.sequence > stamp.sequence => current,
            _ => {
                self.newest_groups.insert(header.subscription_id, stamp);
                stamp
            }
        };
        let (expiry, receiver) = tokio::sync::watch::channel(None);
        self.active_groups.insert(
            key,
            ActiveGroup {
                stamp,
                budget,
                expiry,
            },
        );

        for ((subscription, _), active) in &self.active_groups {
            if *subscription != header.subscription_id || active.stamp.sequence >= newest.sequence {
                continue;
            }
            let deadline = if active.stamp.should_reset(newest, now, active.budget) {
                now
            } else {
                active
                    .stamp
                    .observed_at
                    .checked_add(active.budget)
                    .unwrap_or(now)
            };
            active.expiry.send_replace(Some(deadline));
        }

        Ok(IncomingGroup {
            state,
            key,
            expiry: receiver,
        })
    }
}

/// Connection-scoped native-media control and publishing handle.
///
/// Every event for one admitted connection carries the same handle. Control
/// sends are serialized so subscription ids have one owner and one transition
/// order even when several product tasks react at once.
#[derive(Clone)]
pub struct Session {
    peer: Key,
    connection_id: [u8; 16],
    connection: Arc<dyn comms::Connection>,
    state: Arc<std::sync::Mutex<SessionState>>,
    send_lock: Arc<tokio::sync::Mutex<()>>,
    state_changed: Arc<tokio::sync::Notify>,
    enabled: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("peer", &self.peer)
            .field("connection_id", &self.connection_id)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn new(
        peer: Key,
        connection_id: [u8; 16],
        connection: Arc<dyn comms::Connection>,
        enabled: bool,
    ) -> Self {
        Self {
            peer,
            connection_id,
            connection,
            state: Arc::new(std::sync::Mutex::new(SessionState::default())),
            send_lock: Arc::new(tokio::sync::Mutex::new(())),
            state_changed: Arc::new(tokio::sync::Notify::new()),
            enabled,
        }
    }

    pub fn peer(&self) -> &Key {
        &self.peer
    }

    pub fn connection_id(&self) -> [u8; 16] {
        self.connection_id
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Current transport evidence for encoder adaptation. `Unknown` is a real
    /// answer and should select the conservative bitrate ladder.
    pub fn quality(&self) -> comms::PathQuality {
        self.connection.quality()
    }

    /// Send one state-checked control record on its bounded media flow.
    ///
    /// Once a send begins, its id remains spent even if the transport fails.
    /// Reusing it on a connection whose delivery is unknown would make a late
    /// record apply to a different subscription; callers reconnect instead.
    pub async fn send(&self, control: Control) -> Result<(), Invalid> {
        if !self.enabled {
            return Err(Invalid::FeatureNotNegotiated);
        }
        control.validate()?;
        let _serial = self.send_lock.lock().await;
        self.lock_state().record(Direction::Sent, &control)?;
        self.state_changed.notify_waiters();
        tokio::time::timeout(
            crate::budget::deadline::LIVE_FLOW_READ,
            send_control(self.connection.as_ref(), &control),
        )
        .await
        .map_err(|_| Invalid::Truncated)?
    }

    pub async fn ensure_setup(&self, setup: Setup) -> Result<(), Invalid> {
        if !self.enabled {
            return Ok(());
        }
        let existing = self.lock_state().sent_setup.clone();
        match existing {
            Some(current) if current == setup => Ok(()),
            Some(_) => Err(Invalid::Duplicate),
            None => self.send(Control::Setup(setup)).await,
        }
    }

    /// Begin a Group only for an active subscription owned by this peer.
    /// Newer sequence numbers automatically receive the higher QUIC priority;
    /// product code cannot accidentally invert the live ordering policy.
    pub async fn begin_group(&self, header: GroupHeader) -> Result<OutgoingGroup, Invalid> {
        if !self.enabled {
            return Err(Invalid::FeatureNotNegotiated);
        }
        let _serial = self.send_lock.lock().await;
        self.lock_state()
            .group_budget(Direction::Received, &header)?;
        let priority = i32::try_from(header.group_sequence).unwrap_or(i32::MAX);
        OutgoingGroup::begin(self.connection.as_ref(), header, priority).await
    }

    /// Publish one full independent catalog update on the peer's active
    /// `catalog.json` subscription. The catalog snapshot is spent when the
    /// send begins; after an ambiguous transport failure callers reconnect.
    pub async fn publish_catalog(
        &self,
        header: GroupHeader,
        catalog: Catalog,
    ) -> Result<(), Invalid> {
        if !self.enabled {
            return Err(Invalid::FeatureNotNegotiated);
        }
        if header.track != CATALOG_TRACK
            || header.track_kind != TrackKind::Catalog
            || header.timescale != CATALOG_TIMESCALE
        {
            return Err(Invalid::Bounds);
        }
        let payload = catalog.encode_canonical()?;
        let frame = FrameHeader {
            timestamp: header.published_at_micros.div_euclid(1_000),
            duration: None,
            timescale: CATALOG_TIMESCALE,
            kind: FrameKind::Key,
            payload_len: u32::try_from(payload.len()).map_err(|_| Invalid::TooLarge)?,
        };
        let sequence = header.group_sequence;
        let priority = i32::try_from(sequence).unwrap_or(i32::MAX);
        let _serial = self.send_lock.lock().await;
        {
            let mut state = self.lock_state();
            state.group_budget(Direction::Received, &header)?;
            state.record_catalog_value(Direction::Sent, sequence, catalog)?;
        }
        self.state_changed.notify_waiters();
        let mut group = OutgoingGroup::begin(self.connection.as_ref(), header, priority).await?;
        group.write_frame(frame, &payload).await?;
        group.finish()
    }

    pub(crate) fn accept_control(&self, control: &Control) -> Result<(), Invalid> {
        self.lock_state().record(Direction::Received, control)?;
        self.state_changed.notify_waiters();
        Ok(())
    }

    pub(crate) fn accept_catalog(&self, group: &ReceivedGroup) -> Result<(), Invalid> {
        self.lock_state()
            .record_catalog(Direction::Received, group)?;
        self.state_changed.notify_waiters();
        Ok(())
    }

    pub(crate) async fn begin_received_group(
        &self,
        header: &GroupHeader,
    ) -> Result<IncomingGroup, Invalid> {
        let state = Arc::clone(&self.state);
        let mut deadline = None;
        loop {
            let changed = self.state_changed.notified();
            let admission = {
                let mut locked = self.lock_state();
                match locked.received_group_readiness(header)? {
                    GroupReadiness::Ready => GroupAdmission::Ready(
                        locked.begin_received_group(Arc::clone(&state), header)?,
                    ),
                    GroupReadiness::Pending(budget) => GroupAdmission::Pending(budget),
                }
            };
            match admission {
                GroupAdmission::Ready(group) => return Ok(group),
                GroupAdmission::Pending(budget) => {
                    let until = *deadline.get_or_insert_with(|| {
                        Instant::now()
                            .checked_add(budget)
                            .unwrap_or_else(Instant::now)
                    });
                    tokio::select! {
                        () = tokio::time::sleep_until(until) => return Err(Invalid::StaleGroup),
                        () = changed => {}
                    }
                }
            }
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SessionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Registration for one incomplete incoming Group. Dropping it removes the
/// Group from the connection's bounded active set.
pub(crate) struct IncomingGroup {
    state: Arc<std::sync::Mutex<SessionState>>,
    key: (u64, u64),
    expiry: tokio::sync::watch::Receiver<Option<Instant>>,
}

impl IncomingGroup {
    pub async fn until_stale(&mut self) {
        loop {
            let deadline = *self.expiry.borrow_and_update();
            match deadline {
                Some(deadline) => {
                    tokio::select! {
                        () = tokio::time::sleep_until(deadline) => return,
                        changed = self.expiry.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                    }
                }
                None => {
                    if self.expiry.changed().await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

impl Drop for IncomingGroup {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_groups
            .remove(&self.key);
    }
}

/// One validated media event, bound to the admitted peer and connection that
/// carried it.
#[derive(Debug, Clone)]
pub struct Event {
    pub peer: Key,
    pub connection_id: [u8; 16],
    /// Send replies or publish Groups on this exact admitted connection.
    pub session: Session,
    pub body: EventBody,
}

#[derive(Debug, Clone)]
pub enum EventBody {
    Control(Control),
    Group(Arc<ReceivedGroup>),
}

/// Bounded local handoff from the Live driver to a product-neutral consumer.
#[derive(Clone)]
pub struct Inbox {
    events: tokio::sync::broadcast::Sender<Event>,
}

impl Inbox {
    pub fn new() -> Self {
        Self {
            events: tokio::sync::broadcast::channel(EVENT_QUEUE).0,
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub(crate) fn publish(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

impl Default for Inbox {
    fn default() -> Self {
        Self::new()
    }
}

/// Open and write one bounded control/feedback flow.
pub async fn send_control(
    connection: &dyn comms::Connection,
    control: &Control,
) -> Result<(), Invalid> {
    let body = control.encode()?;
    let (mut send, _recv) = connection.open_bi().await.map_err(|_| Invalid::Truncated)?;
    send.write_all(&[stream_kind::MEDIA_CONTROL])
        .await
        .map_err(|_| Invalid::Truncated)?;
    write_framed(send.as_mut(), &body).await?;
    send.finish().map_err(|_| Invalid::Truncated)
}

/// Read a media control after its lane byte has already been consumed.
pub async fn read_control_body(flow: &mut dyn comms::RecvFlow) -> Result<Control, Invalid> {
    let body =
        crate::plane_stream::read_framed(flow, crate::plane::bounds::MAX_CONTROL_FRAME_BYTES)
            .await
            .map_err(|_| Invalid::Truncated)?;
    Control::decode_canonical(&body)
}

/// A Group being written to its own QUIC unidirectional stream.
pub struct OutgoingGroup {
    send: Box<dyn comms::SendFlow>,
    header: GroupHeader,
    frames: usize,
    bytes: usize,
    first_timestamp: Option<i64>,
    last_timestamp: Option<i64>,
}

impl OutgoingGroup {
    /// `priority` is advisory and resolved by the publisher. A live publisher
    /// gives a newer Group a larger value so old Groups cannot head-of-line it.
    pub async fn begin(
        connection: &dyn comms::Connection,
        header: GroupHeader,
        priority: i32,
    ) -> Result<Self, Invalid> {
        let body = header.encode()?;
        let mut send = connection
            .open_uni()
            .await
            .map_err(|_| Invalid::Truncated)?;
        send.set_priority(priority);
        send.write_all(&[stream_kind::MEDIA_GROUP])
            .await
            .map_err(|_| Invalid::Truncated)?;
        write_framed(send.as_mut(), &body).await?;
        Ok(Self {
            send,
            header,
            frames: 0,
            bytes: body.len(),
            first_timestamp: None,
            last_timestamp: None,
        })
    }

    pub async fn write_frame(
        &mut self,
        header: FrameHeader,
        payload: &[u8],
    ) -> Result<(), Invalid> {
        if self.frames >= MAX_FRAMES_PER_GROUP {
            return Err(Invalid::TooLarge);
        }
        let declared = usize::try_from(header.payload_len).map_err(|_| Invalid::Bounds)?;
        if declared != payload.len() {
            return Err(Invalid::PayloadLength);
        }
        if header.timescale != self.header.timescale {
            return Err(Invalid::Bounds);
        }
        if self.frames == 0 && header.kind != FrameKind::Key {
            return Err(Invalid::FirstFrameNotKey);
        }
        if self.header.track_kind == TrackKind::Catalog {
            if self.frames != 0 || payload.len() > MAX_CATALOG_BYTES {
                return Err(Invalid::TooLarge);
            }
            Catalog::decode_canonical(payload)?;
        }
        validate_timestamp(
            self.first_timestamp,
            self.last_timestamp,
            &header,
            self.header.max_group_duration_ms,
        )?;
        let encoded = header.encode()?;
        let added = encoded
            .len()
            .checked_add(payload.len())
            .ok_or(Invalid::TooLarge)?;
        if self.bytes.saturating_add(added) > MAX_MEDIA_GROUP_BYTES {
            return Err(Invalid::TooLarge);
        }
        write_framed(self.send.as_mut(), &encoded).await?;
        self.send
            .write_all(payload)
            .await
            .map_err(|_| Invalid::Truncated)?;
        self.frames = self.frames.saturating_add(1);
        self.bytes = self.bytes.saturating_add(added);
        self.first_timestamp.get_or_insert(header.timestamp);
        self.last_timestamp = Some(header.timestamp);
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), Invalid> {
        if self.frames == 0 || (self.header.track_kind == TrackKind::Catalog && self.frames != 1) {
            self.send.reset(RESET_MEDIA);
            return Err(Invalid::FirstFrameNotKey);
        }
        self.send.finish().map_err(|_| Invalid::Truncated)
    }

    pub fn reset(mut self) {
        self.send.reset(RESET_MEDIA);
    }
}

/// Read a complete Group after the `0x03` lane byte has been consumed.
///
/// The production session runs one of these per task, so an old Group waiting
/// on bytes cannot prevent the accept loop from admitting a newer stream.
pub async fn read_group_body(flow: &mut dyn comms::RecvFlow) -> Result<ReceivedGroup, Invalid> {
    let header = read_group_header(flow).await?;
    read_group_frames(flow, header).await
}

/// Read and validate the first record before any frame payload is allocated.
pub async fn read_group_header(flow: &mut dyn comms::RecvFlow) -> Result<GroupHeader, Invalid> {
    let header_bytes = read_required_framed(flow, MAX_MEDIA_HEADER_BYTES).await?;
    GroupHeader::decode_canonical(&header_bytes)
}

/// Read a Group's ordered Frames after its subscription-bearing header has
/// been admitted by the connection state machine.
pub async fn read_group_frames(
    flow: &mut dyn comms::RecvFlow,
    header: GroupHeader,
) -> Result<ReceivedGroup, Invalid> {
    let mut frames = Vec::new();
    let mut bytes = header.encode()?.len();
    let mut first_timestamp = None;
    let mut last_timestamp = None;

    loop {
        let Some(frame_bytes) = read_optional_framed(flow, MAX_MEDIA_HEADER_BYTES).await? else {
            break;
        };
        if frames.len() >= MAX_FRAMES_PER_GROUP {
            flow.stop(RESET_MEDIA);
            return Err(Invalid::TooLarge);
        }
        let frame_header = FrameHeader::decode_canonical(&frame_bytes)?;
        if frame_header.timescale != header.timescale {
            flow.stop(RESET_MEDIA);
            return Err(Invalid::Bounds);
        }
        if frames.is_empty() && frame_header.kind != FrameKind::Key {
            flow.stop(RESET_MEDIA);
            return Err(Invalid::FirstFrameNotKey);
        }
        validate_timestamp(
            first_timestamp,
            last_timestamp,
            &frame_header,
            header.max_group_duration_ms,
        )?;
        let payload_len = usize::try_from(frame_header.payload_len).map_err(|_| Invalid::Bounds)?;
        if header.track_kind == TrackKind::Catalog
            && (!frames.is_empty() || payload_len > MAX_CATALOG_BYTES)
        {
            flow.stop(RESET_MEDIA);
            return Err(Invalid::TooLarge);
        }
        let added = frame_bytes
            .len()
            .checked_add(payload_len)
            .ok_or(Invalid::TooLarge)?;
        if bytes.saturating_add(added) > MAX_MEDIA_GROUP_BYTES {
            flow.stop(RESET_MEDIA);
            return Err(Invalid::TooLarge);
        }
        let payload = flow
            .read_exact(payload_len)
            .await
            .map_err(|_| Invalid::Truncated)?;
        first_timestamp.get_or_insert(frame_header.timestamp);
        last_timestamp = Some(frame_header.timestamp);
        bytes = bytes.saturating_add(added);
        frames.push(Frame {
            header: frame_header,
            payload,
        });
    }
    if frames.is_empty() {
        return Err(Invalid::FirstFrameNotKey);
    }
    let group = ReceivedGroup { header, frames };
    if group.header.track_kind == TrackKind::Catalog {
        Catalog::from_group(&group)?;
    }
    Ok(group)
}

/// The two clocks used to decide whether an incomplete Group is now stale.
#[derive(Debug, Clone, Copy)]
pub struct GroupStamp {
    pub sequence: u64,
    pub published_at_micros: i64,
    pub observed_at: Instant,
}

impl GroupStamp {
    /// An older Group expires after a newer one exists and either the shared
    /// timeline or local monotonic time reaches the latency budget. This is the
    /// earlier of the two deadlines; wall-clock skew cannot buy an old stream
    /// more flow-control time.
    pub fn should_reset(self, newest: Self, now: Instant, budget: Duration) -> bool {
        if newest.sequence <= self.sequence {
            return false;
        }
        let wall_expired = now.saturating_duration_since(self.observed_at) >= budget;
        let timeline_age = newest
            .published_at_micros
            .checked_sub(self.published_at_micros)
            .and_then(|age| u64::try_from(age).ok())
            .map(Duration::from_micros);
        wall_expired || timeline_age.is_some_and(|age| age >= budget)
    }
}

fn validate_track(track: &str) -> Result<(), Invalid> {
    if track.is_empty()
        || track.len() > MAX_TRACK_NAME_BYTES
        || track.chars().any(char::is_control)
        || track.starts_with('/')
        || track.contains("..")
        || track.contains("://")
    {
        return Err(Invalid::Bounds);
    }
    Ok(())
}

fn validate_codec(codec: &str) -> Result<(), Invalid> {
    if codec.is_empty()
        || codec.len() > MAX_CODEC_NAME_BYTES
        || !codec.is_ascii()
        || codec
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || byte == b'.'))
    {
        return Err(Invalid::Bounds);
    }
    Ok(())
}

fn codec_family(kind: TrackKind, codec: &str) -> Result<CodecFamily, Invalid> {
    validate_codec(codec)?;
    let family = match kind {
        TrackKind::Video if is_avc_codec(codec) => CodecFamily::H264,
        TrackKind::Video if has_codec_suffix(codec, "av01.") => CodecFamily::Av1,
        TrackKind::Audio if matches!(codec, "mp4a.40.2" | "mp4a.40.02" | "mp4a.67") => {
            CodecFamily::Aac
        }
        TrackKind::Video | TrackKind::Audio | TrackKind::Catalog => {
            return Err(Invalid::UnsupportedCodec)
        }
    };
    Ok(family)
}

fn is_avc_codec(codec: &str) -> bool {
    ["avc1.", "avc3."].iter().any(|prefix| {
        codec.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.len() == 6 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    })
}

fn has_codec_suffix(codec: &str, prefix: &str) -> bool {
    codec
        .strip_prefix(prefix)
        .is_some_and(|suffix| !suffix.is_empty())
}

fn validate_opaque_id(value: &str, max: usize) -> Result<(), Invalid> {
    if value.is_empty()
        || value.len() > max
        || !value.is_ascii()
        || value.bytes().any(|byte| {
            !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(Invalid::Bounds);
    }
    Ok(())
}

fn validate_latency(latency_ms: u32) -> Result<(), Invalid> {
    if latency_ms == 0 || latency_ms > MAX_LATENCY_MS {
        return Err(Invalid::Bounds);
    }
    Ok(())
}

fn validate_durations(group_ms: u32, latency_ms: u32) -> Result<(), Invalid> {
    if group_ms == 0 || group_ms > MAX_GROUP_DURATION_MS {
        return Err(Invalid::Bounds);
    }
    validate_latency(latency_ms)
}

fn validate_range(start: Option<u64>, end: Option<u64>) -> Result<(), Invalid> {
    if matches!((start, end), (Some(first), Some(last)) if first > last) {
        return Err(Invalid::Bounds);
    }
    Ok(())
}

fn validate_timestamp(
    first: Option<i64>,
    last: Option<i64>,
    header: &FrameHeader,
    max_group_duration_ms: u32,
) -> Result<(), Invalid> {
    if last.is_some_and(|previous| header.timestamp < previous) {
        return Err(Invalid::TimestampOrder);
    }
    let Some(first) = first else {
        return Ok(());
    };
    let start_ticks = header
        .timestamp
        .checked_sub(first)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(Invalid::TimestampOrder)?;
    let end_ticks = start_ticks
        .checked_add(header.duration.unwrap_or(0))
        .ok_or(Invalid::GroupDuration)?;
    let elapsed_ms = u128::from(end_ticks)
        .checked_mul(1_000)
        .ok_or(Invalid::GroupDuration)?
        / u128::from(header.timescale);
    if elapsed_ms > u128::from(max_group_duration_ms) {
        return Err(Invalid::GroupDuration);
    }
    Ok(())
}

fn encode_body<T: Serialize>(value: &T) -> Result<Vec<u8>, Invalid> {
    postcard::to_stdvec(value).map_err(|_| Invalid::NonCanonical)
}

fn decode_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Invalid> {
    postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)
}

async fn write_framed(flow: &mut dyn comms::SendFlow, bytes: &[u8]) -> Result<(), Invalid> {
    let len = u32::try_from(bytes.len()).map_err(|_| Invalid::TooLarge)?;
    flow.write_all(&len.to_le_bytes())
        .await
        .map_err(|_| Invalid::Truncated)?;
    flow.write_all(bytes).await.map_err(|_| Invalid::Truncated)
}

async fn read_required_framed(
    flow: &mut dyn comms::RecvFlow,
    max: usize,
) -> Result<Vec<u8>, Invalid> {
    read_optional_framed(flow, max)
        .await?
        .ok_or(Invalid::Truncated)
}

async fn read_optional_framed(
    flow: &mut dyn comms::RecvFlow,
    max: usize,
) -> Result<Option<Vec<u8>>, Invalid> {
    let mut prefix = loop {
        let Some(first) = flow.read_chunk(4).await.map_err(|_| Invalid::Truncated)? else {
            return Ok(None);
        };
        if !first.is_empty() {
            break first;
        }
    };
    if prefix.len() > 4 {
        return Err(Invalid::NonCanonical);
    }
    while prefix.len() < 4 {
        let want = 4usize.saturating_sub(prefix.len());
        let more = flow
            .read_chunk(want)
            .await
            .map_err(|_| Invalid::Truncated)?
            .ok_or(Invalid::Truncated)?;
        if more.is_empty() {
            continue;
        }
        prefix.extend_from_slice(&more);
    }
    let prefix: [u8; 4] = prefix.try_into().map_err(|_| Invalid::NonCanonical)?;
    let len = usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| Invalid::TooLarge)?;
    if len > max {
        return Err(Invalid::TooLarge);
    }
    flow.read_exact(len)
        .await
        .map(Some)
        .map_err(|_| Invalid::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use comms::Transport as _;

    fn group_header(sequence: u64) -> GroupHeader {
        GroupHeader {
            subscription_id: 7,
            track: "screen/main".into(),
            track_kind: TrackKind::Video,
            group_sequence: sequence,
            published_at_micros: 1_000_000,
            timescale: 90_000,
            max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
        }
    }

    fn setup() -> Setup {
        Setup {
            protocol_version: PROTOCOL_VERSION,
            max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            max_latency_ms: DEFAULT_MAX_LATENCY_MS,
        }
    }

    fn subscribe() -> Subscribe {
        Subscribe {
            subscription_id: 7,
            track: "screen/main".into(),
            priority: 9,
            ordered: false,
            max_latency_ms: DEFAULT_MAX_LATENCY_MS,
            start_group: None,
            end_group: None,
        }
    }

    fn catalog_subscribe() -> Subscribe {
        Subscribe {
            subscription_id: 2,
            track: CATALOG_TRACK.into(),
            priority: u8::MAX,
            ordered: false,
            max_latency_ms: DEFAULT_MAX_LATENCY_MS,
            start_group: None,
            end_group: None,
        }
    }

    fn track_info() -> TrackInfo {
        TrackInfo {
            track: "screen/main".into(),
            kind: TrackKind::Video,
            codec: "avc1.640028".into(),
            timescale: 90_000,
            decoder_config: vec![1, 100, 0, 40],
            max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
        }
    }

    fn catalog_track_info() -> TrackInfo {
        TrackInfo {
            track: CATALOG_TRACK.into(),
            kind: TrackKind::Catalog,
            codec: CATALOG_CODEC.into(),
            timescale: CATALOG_TIMESCALE,
            decoder_config: Vec::new(),
            max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            version: CATALOG_VERSION,
            jitter_hint_ms: 250,
            tracks: vec![
                CatalogTrack {
                    track: "screen/main".into(),
                    kind: TrackKind::Video,
                    codec: "avc1.640028".into(),
                    timescale: 90_000,
                    decoder_config_hex: "01640028".into(),
                    max_group_duration_ms: 2_000,
                    target_latency_ms: 2_000,
                    bitrate_bps: 4_000_000,
                    width: Some(1_920),
                    height: Some(1_080),
                    frame_rate_milli: Some(60_000),
                    sample_rate: None,
                    channels: None,
                    render_group: Some("main".into()),
                    cmaf_rendition: Some("main_h264".into()),
                    hls_v3_rendition: Some("main_h264".into()),
                },
                CatalogTrack {
                    track: "audio/aac".into(),
                    kind: TrackKind::Audio,
                    codec: "mp4a.40.2".into(),
                    timescale: 48_000,
                    decoder_config_hex: "1190".into(),
                    max_group_duration_ms: 2_000,
                    target_latency_ms: 2_000,
                    bitrate_bps: 128_000,
                    width: None,
                    height: None,
                    frame_rate_milli: None,
                    sample_rate: Some(48_000),
                    channels: Some(2),
                    render_group: Some("main".into()),
                    cmaf_rendition: Some("main_aac".into()),
                    hls_v3_rendition: Some("main_aac".into()),
                },
                CatalogTrack {
                    track: "video/av1".into(),
                    kind: TrackKind::Video,
                    codec: "av01.0.08M.10.0.110.09".into(),
                    timescale: 90_000,
                    decoder_config_hex: String::new(),
                    max_group_duration_ms: 2_000,
                    target_latency_ms: 2_000,
                    bitrate_bps: 2_500_000,
                    width: Some(1_920),
                    height: Some(1_080),
                    frame_rate_milli: Some(60_000),
                    sample_rate: None,
                    channels: None,
                    render_group: Some("main".into()),
                    cmaf_rendition: Some("main_av1".into()),
                    hls_v3_rendition: None,
                },
            ],
        }
    }

    fn catalog_group(sequence: u64) -> ReceivedGroup {
        let payload = catalog().encode_canonical().expect("catalog");
        ReceivedGroup {
            header: GroupHeader {
                subscription_id: 2,
                track: CATALOG_TRACK.into(),
                track_kind: TrackKind::Catalog,
                group_sequence: sequence,
                published_at_micros: 1_000_000,
                timescale: CATALOG_TIMESCALE,
                max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            },
            frames: vec![Frame {
                header: FrameHeader {
                    timestamp: 1_000,
                    duration: None,
                    timescale: CATALOG_TIMESCALE,
                    kind: FrameKind::Key,
                    payload_len: u32::try_from(payload.len()).expect("bounded"),
                },
                payload,
            }],
        }
    }

    async fn accept_media_control(connection: &dyn comms::Connection) -> Control {
        loop {
            let (mut answer, mut flow) = connection
                .accept_bi()
                .await
                .expect("accept control")
                .expect("control flow");
            let lane = flow
                .read_exact(1)
                .await
                .expect("control lane")
                .first()
                .copied()
                .expect("one lane byte");
            if lane == stream_kind::MEDIA_CONTROL {
                let control = read_control_body(flow.as_mut())
                    .await
                    .expect("media control");
                answer.finish().expect("finish media answer");
                return control;
            }
            flow.read_to_end(crate::plane::bounds::MAX_CONTROL_FRAME_BYTES)
                .await
                .expect("drain other control");
            answer.finish().expect("finish other answer");
        }
    }

    #[test]
    fn every_control_selector_round_trips_canonically() {
        let values = [
            Control::Setup(Setup {
                protocol_version: PROTOCOL_VERSION,
                max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
                max_latency_ms: 3_000,
            }),
            Control::Subscribe(Subscribe {
                subscription_id: 1,
                track: "screen/main".into(),
                priority: 9,
                ordered: false,
                max_latency_ms: 3_000,
                start_group: None,
                end_group: None,
            }),
            Control::SubscribeUpdate(SubscribeUpdate {
                subscription_id: 1,
                priority: 10,
                ordered: false,
                max_latency_ms: 2_500,
                start_group: Some(4),
                end_group: None,
            }),
            Control::SubscribeOk(SubscribeOk {
                subscription_id: 1,
                publisher_priority: 8,
                publisher_max_latency_ms: 2_000,
                start_group: Some(4),
                end_group: None,
            }),
            Control::SubscribeDrop(SubscribeDrop {
                subscription_id: 1,
                start_group: 4,
                end_group: 6,
                error_code: 0,
            }),
            Control::SubscribeEnd(SubscribeEnd { subscription_id: 1 }),
            Control::Fetch(Fetch {
                fetch_id: 3,
                track: "screen/main".into(),
                group_sequence: 8,
                priority: 7,
            }),
            Control::TrackInfo(TrackInfo {
                track: "screen/main".into(),
                kind: TrackKind::Video,
                codec: "avc1.640028".into(),
                timescale: 90_000,
                decoder_config: vec![1, 100, 0, 40],
                max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            }),
            Control::RequestKeyframe(RequestKeyframe {
                track: "screen/main".into(),
            }),
            Control::ClockProbe(ClockProbe {
                probe_id: 12,
                t0_micros: 100,
            }),
            Control::ClockReply(ClockReply {
                probe_id: 12,
                t0_micros: 100,
                t1_micros: 120,
                t2_micros: 122,
            }),
            Control::PlayoutTarget(PlayoutTarget {
                track: "screen/main".into(),
                shared_time_micros: 10_000,
                position_timestamp: 90_000,
                timescale: 90_000,
                playout_delay_ms: 1_500,
            }),
            Control::GoAway(GoAway {
                retry_after_ms: Some(1_000),
            }),
        ];
        let mut selectors = std::collections::BTreeSet::new();
        for value in values {
            let encoded = value.encode().expect("valid control");
            selectors.insert(encoded[0]);
            assert_eq!(Control::decode_canonical(&encoded), Ok(value));
        }
        assert_eq!(selectors.len(), 13, "selectors are explicit and unique");
    }

    #[test]
    fn track_names_are_identifiers_and_not_routes() {
        for bad in ["", "/root", "../track", "https://elsewhere.invalid/live"] {
            let mut header = group_header(1);
            header.track = bad.into();
            assert_eq!(header.encode(), Err(Invalid::Bounds), "{bad}");
        }
    }

    #[test]
    fn generation_one_headers_have_frozen_encodings() {
        let setup = Control::Setup(Setup {
            protocol_version: PROTOCOL_VERSION,
            max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            max_latency_ms: 3_000,
        })
        .encode()
        .expect("setup");
        let group = group_header(42).encode().expect("group");
        let frame = FrameHeader {
            timestamp: 90_000,
            duration: Some(3_000),
            timescale: 90_000,
            kind: FrameKind::Key,
            payload_len: 15,
        }
        .encode()
        .expect("frame");
        assert_eq!(hex(&setup), "0101d00fb817");
        assert_eq!(
            hex(&group),
            "070b73637265656e2f6d61696e002a80897a90bf05d00f"
        );
        assert_eq!(hex(&frame), "a0fe0a01b81790bf05000f");
    }

    #[test]
    fn catalog_json_is_canonical_bounded_and_codec_selectable() {
        let catalog = catalog();
        let encoded = catalog.encode_canonical().expect("catalog");
        assert_eq!(Catalog::decode_canonical(&encoded), Ok(catalog.clone()));
        assert_eq!(
            std::str::from_utf8(&encoded).expect("utf8"),
            concat!(
                r#"{"version":1,"jitter_hint_ms":250,"tracks":["#,
                r#"{"track":"screen/main","kind":"video","codec":"avc1.640028","timescale":90000,"decoder_config_hex":"01640028","max_group_duration_ms":2000,"target_latency_ms":2000,"bitrate_bps":4000000,"width":1920,"height":1080,"frame_rate_milli":60000,"render_group":"main","cmaf_rendition":"main_h264","hls_v3_rendition":"main_h264"},"#,
                r#"{"track":"audio/aac","kind":"audio","codec":"mp4a.40.2","timescale":48000,"decoder_config_hex":"1190","max_group_duration_ms":2000,"target_latency_ms":2000,"bitrate_bps":128000,"sample_rate":48000,"channels":2,"render_group":"main","cmaf_rendition":"main_aac","hls_v3_rendition":"main_aac"},"#,
                r#"{"track":"video/av1","kind":"video","codec":"av01.0.08M.10.0.110.09","timescale":90000,"decoder_config_hex":"","max_group_duration_ms":2000,"target_latency_ms":2000,"bitrate_bps":2500000,"width":1920,"height":1080,"frame_rate_milli":60000,"render_group":"main","cmaf_rendition":"main_av1"}]}"#
            )
        );
        let mut padded = encoded;
        padded.push(b'\n');
        assert_eq!(
            Catalog::decode_canonical(&padded),
            Err(Invalid::NonCanonical)
        );

        let baseline = DecoderSupport::baseline();
        assert!(baseline.supports(&catalog.tracks[0]));
        assert!(baseline.supports(&catalog.tracks[1]));
        assert!(!baseline.supports(&catalog.tracks[2]));
        assert!(baseline.with_av1().supports(&catalog.tracks[2]));

        let info = catalog.tracks[0].track_info().expect("track info");
        info.matches_catalog(&catalog.tracks[0])
            .expect("exact decoder configuration");
        let mut changed = info;
        changed.timescale = 1_000;
        assert_eq!(
            changed.matches_catalog(&catalog.tracks[0]),
            Err(Invalid::TrackInfoMismatch)
        );
    }

    #[test]
    fn catalog_requires_baseline_tracks_and_opaque_http_renditions() {
        let mut no_h264 = catalog();
        no_h264.tracks.remove(0);
        assert_eq!(no_h264.encode_canonical(), Err(Invalid::BaselineRequired));

        let mut external = catalog();
        external.tracks[0].cmaf_rendition = Some("https://media.invalid/live".into());
        assert_eq!(external.encode_canonical(), Err(Invalid::Bounds));

        let mut av1_hls = catalog();
        av1_hls.tracks[2].hls_v3_rendition = Some("main_av1".into());
        assert_eq!(av1_hls.encode_canonical(), Err(Invalid::UnsupportedCodec));

        let mut incomplete_video = catalog();
        incomplete_video.tracks[0].frame_rate_milli = None;
        assert_eq!(incomplete_video.encode_canonical(), Err(Invalid::Bounds));
        assert_eq!(
            SessionState::default().record_catalog_value(Direction::Received, 1, incomplete_video),
            Err(Invalid::Bounds)
        );
    }

    #[test]
    fn a_catalog_update_is_exactly_one_key_frame_in_one_group() {
        let group = catalog_group(9);
        assert_eq!(Catalog::from_group(&group), Ok(catalog()));

        let mut duplicate = group;
        let frame = duplicate.frames.first().expect("one Frame").clone();
        duplicate.frames.push(frame);
        assert_eq!(Catalog::from_group(&duplicate), Err(Invalid::Bounds));

        let mut duration = catalog_group(10);
        duration.frames[0].header.duration = Some(1);
        assert_eq!(Catalog::from_group(&duration), Err(Invalid::Bounds));
    }

    #[allow(clippy::format_collect, reason = "small test-only wire fixture")]
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn a_frame_duration_cannot_cross_the_group_cap() {
        let frame = FrameHeader {
            timestamp: 179_900,
            duration: Some(200),
            timescale: 90_000,
            kind: FrameKind::Delta,
            payload_len: 1,
        };
        assert_eq!(
            validate_timestamp(Some(0), Some(170_000), &frame, 2_000),
            Err(Invalid::GroupDuration)
        );
    }

    #[test]
    fn the_earlier_expiry_clock_resets_an_old_group_only_after_a_new_one_exists() {
        let now = Instant::now();
        let old = GroupStamp {
            sequence: 3,
            published_at_micros: 1_000_000,
            observed_at: now,
        };
        assert!(!old.should_reset(old, now + Duration::from_secs(10), Duration::from_secs(2)));
        let newer_by_timeline = GroupStamp {
            sequence: 4,
            published_at_micros: 3_500_000,
            observed_at: now + Duration::from_millis(10),
        };
        assert!(old.should_reset(
            newer_by_timeline,
            now + Duration::from_millis(10),
            Duration::from_secs(2)
        ));

        let newer_by_wall = GroupStamp {
            sequence: 4,
            published_at_micros: 1_100_000,
            observed_at: now + Duration::from_secs(3),
        };
        assert!(old.should_reset(
            newer_by_wall,
            now + Duration::from_secs(3),
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn subscriptions_have_one_owner_one_setup_and_spent_ids() {
        let mut state = SessionState::default();
        assert_eq!(
            state.record(Direction::Sent, &Control::Subscribe(subscribe())),
            Err(Invalid::SetupRequired)
        );
        state
            .record(Direction::Sent, &Control::Setup(setup()))
            .expect("local setup");
        state
            .record(Direction::Received, &Control::Setup(setup()))
            .expect("remote setup");
        state
            .record(Direction::Sent, &Control::Subscribe(subscribe()))
            .expect("subscribe");
        assert_eq!(
            state.record(Direction::Sent, &Control::Subscribe(subscribe())),
            Err(Invalid::Duplicate)
        );
        state
            .record(
                Direction::Received,
                &Control::SubscribeOk(SubscribeOk {
                    subscription_id: 7,
                    publisher_priority: 4,
                    publisher_max_latency_ms: DEFAULT_MAX_LATENCY_MS,
                    start_group: None,
                    end_group: None,
                }),
            )
            .expect("publisher accepted");
        state
            .record_catalog_value(Direction::Received, 1, catalog())
            .expect("catalog");
        let mut mismatch = track_info();
        mismatch.decoder_config.push(0);
        assert_eq!(
            state.record(Direction::Received, &Control::TrackInfo(mismatch)),
            Err(Invalid::TrackInfoMismatch)
        );
        state
            .record(Direction::Received, &Control::TrackInfo(track_info()))
            .expect("decoder configuration");
        assert_eq!(
            state.group_budget(Direction::Sent, &group_header(1)),
            Ok(Duration::from_millis(u64::from(DEFAULT_MAX_LATENCY_MS)))
        );
        let mut wrong_track = group_header(1);
        wrong_track.track = "screen/other".into();
        assert_eq!(
            state.group_budget(Direction::Sent, &wrong_track),
            Err(Invalid::WrongTrack)
        );
        state
            .record(
                Direction::Received,
                &Control::SubscribeEnd(SubscribeEnd { subscription_id: 7 }),
            )
            .expect("publisher ended");
        assert_eq!(
            state.group_budget(Direction::Sent, &group_header(2)),
            Err(Invalid::SubscriptionNotActive)
        );
    }

    #[test]
    fn setup_negotiates_the_narrower_latency_and_group_caps() {
        let mut state = SessionState::default();
        state
            .record(Direction::Sent, &Control::Setup(setup()))
            .expect("local setup");
        let mut narrower = setup();
        narrower.max_group_duration_ms = 1_000;
        narrower.max_latency_ms = 1_000;
        state
            .record(Direction::Received, &Control::Setup(narrower))
            .expect("remote setup");
        assert_eq!(
            state.record(Direction::Sent, &Control::Subscribe(subscribe())),
            Err(Invalid::Bounds)
        );
        let mut request = subscribe();
        request.max_latency_ms = 1_000;
        state
            .record(Direction::Sent, &Control::Subscribe(request))
            .expect("narrow subscription");
        assert_eq!(
            state.record(
                Direction::Received,
                &Control::SubscribeOk(SubscribeOk {
                    subscription_id: 7,
                    publisher_priority: 4,
                    publisher_max_latency_ms: 1_500,
                    start_group: None,
                    end_group: None,
                })
            ),
            Err(Invalid::Bounds)
        );
        state
            .record(
                Direction::Received,
                &Control::SubscribeOk(SubscribeOk {
                    subscription_id: 7,
                    publisher_priority: 4,
                    publisher_max_latency_ms: 1_000,
                    start_group: None,
                    end_group: None,
                }),
            )
            .expect("narrow response");
        let mut offered = catalog();
        offered.tracks[0].max_group_duration_ms = 1_000;
        state
            .record_catalog_value(Direction::Received, 1, offered)
            .expect("catalog");
        let mut info = track_info();
        info.max_group_duration_ms = 1_000;
        state
            .record(Direction::Received, &Control::TrackInfo(info))
            .expect("narrow track info");
        assert_eq!(
            state.group_budget(Direction::Sent, &group_header(1)),
            Err(Invalid::Bounds)
        );
        let mut group = group_header(1);
        group.max_group_duration_ms = 1_000;
        assert_eq!(
            state.group_budget(Direction::Sent, &group),
            Ok(Duration::from_secs(1))
        );
    }

    #[test]
    fn ended_subscription_ids_bound_connection_lifetime_churn() {
        let mut state = SessionState::default();
        state
            .record(Direction::Sent, &Control::Setup(setup()))
            .expect("local setup");
        state
            .record(Direction::Received, &Control::Setup(setup()))
            .expect("remote setup");
        for subscription_id in 0..MAX_SUBSCRIPTIONS_PER_SESSION {
            let mut request = subscribe();
            request.subscription_id = u64::try_from(subscription_id).expect("small bound");
            state
                .record(Direction::Sent, &Control::Subscribe(request))
                .expect("bounded subscription");
            state
                .record(
                    Direction::Received,
                    &Control::SubscribeEnd(SubscribeEnd {
                        subscription_id: u64::try_from(subscription_id).expect("small bound"),
                    }),
                )
                .expect("ended subscription");
        }
        let mut overflow = subscribe();
        overflow.subscription_id =
            u64::try_from(MAX_SUBSCRIPTIONS_PER_SESSION).expect("small bound");
        assert_eq!(
            state.record(Direction::Sent, &Control::Subscribe(overflow)),
            Err(Invalid::TooLarge)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_newer_group_arms_the_older_groups_monotonic_deadline() {
        let state = Arc::new(std::sync::Mutex::new(SessionState::default()));
        {
            let mut locked = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locked
                .record(Direction::Sent, &Control::Setup(setup()))
                .expect("local setup");
            locked
                .record(Direction::Received, &Control::Setup(setup()))
                .expect("remote setup");
            let mut request = subscribe();
            request.max_latency_ms = 50;
            locked
                .record(Direction::Sent, &Control::Subscribe(request))
                .expect("subscribe");
            locked
                .record(
                    Direction::Received,
                    &Control::SubscribeOk(SubscribeOk {
                        subscription_id: 7,
                        publisher_priority: 4,
                        publisher_max_latency_ms: 50,
                        start_group: None,
                        end_group: None,
                    }),
                )
                .expect("active");
            locked
                .record_catalog_value(Direction::Received, 1, catalog())
                .expect("catalog");
            locked
                .record(Direction::Received, &Control::TrackInfo(track_info()))
                .expect("decoder configuration");
        }
        let mut old = {
            let mut locked = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locked
                .begin_received_group(Arc::clone(&state), &group_header(1))
                .expect("old Group")
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(1), old.until_stale())
                .await
                .is_err(),
            "age alone cannot expire the newest Group"
        );
        let newer = {
            let mut header = group_header(2);
            header.published_at_micros = 1_001_000;
            let mut locked = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locked
                .begin_received_group(Arc::clone(&state), &header)
                .expect("new Group")
        };
        tokio::time::advance(Duration::from_millis(50)).await;
        tokio::time::timeout(Duration::from_millis(1), old.until_stale())
            .await
            .expect("the armed monotonic deadline retires the old Group");

        drop(newer);
        let completed_newest = {
            let mut header = group_header(4);
            header.published_at_micros = 2_000_000;
            let mut locked = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locked
                .begin_received_group(Arc::clone(&state), &header)
                .expect("newest Group")
        };
        drop(completed_newest);
        let mut late_old = {
            let mut header = group_header(3);
            header.published_at_micros = 1_000_000;
            let mut locked = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locked
                .begin_received_group(Arc::clone(&state), &header)
                .expect("late old Group")
        };
        tokio::time::timeout(Duration::from_millis(1), late_old.until_stale())
            .await
            .expect("completed newer Groups still retire late old streams");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_live_session_dispatches_a_validated_group_to_its_bounded_inbox() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let net = comms::mem::MemNet::new();
                let dialer_device = mechanics::actor::device_from_seed(&[81u8; 32]);
                let accepter_device = mechanics::actor::device_from_seed(&[82u8; 32]);
                let dialer_transport = net.peer(dialer_device.clone());
                let accepter_transport = net.peer(accepter_device);
                let accepting = async { accepter_transport.accept_connection().await };
                let dialling = dialer_transport
                    .connect_session(accepter_transport.my_id(), crate::plane::LIVE_ALPN);
                let (dialer, incoming) = tokio::join!(dialling, accepting);
                let dialer = dialer.expect("connect");
                let incoming = incoming.expect("incoming");

                let handle = Arc::new(super::super::LiveHandle::new(None));
                let mut events = handle.media();
                let cancel = crate::lifecycle::CancelToken::new();
                let station = Key::from_device(&dialer_device).expect("station");
                let peer = crate::admission::AdmittedPeer {
                    station: station.clone(),
                    actor: mechanics::ids::ActorId::parse(&format!("act_{}", "ab".repeat(32)))
                        .expect("actor"),
                    authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(
                        vec![1],
                    ),
                    granted_lanes: vec![stream_kind::MEDIA_GROUP, stream_kind::MEDIA_CONTROL],
                    connection_id: [9u8; 16],
                    connection_epoch: [10u8; 16],
                    features: crate::plane::feature::NATIVE_LIVE_MEDIA,
                };
                let serving = super::super::serve_session(
                    Arc::from(incoming.connection),
                    peer,
                    cancel.clone(),
                    super::super::Context {
                        handle: Some(Arc::clone(&handle)),
                        signals: None,
                        worlds: None,
                        authority: None,
                    },
                );
                let sending = async {
                    assert_eq!(
                        accept_media_control(dialer.as_ref()).await,
                        Control::Setup(setup())
                    );
                    send_control(dialer.as_ref(), &Control::Setup(setup()))
                        .await
                        .expect("peer setup");
                    let setup_event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .expect("setup event in time")
                        .expect("setup event");
                    let session = setup_event.session;
                    assert!(matches!(
                        setup_event.body,
                        EventBody::Control(Control::Setup(_))
                    ));

                    session
                        .send(Control::Subscribe(catalog_subscribe()))
                        .await
                        .expect("subscribe catalog");
                    assert_eq!(
                        accept_media_control(dialer.as_ref()).await,
                        Control::Subscribe(catalog_subscribe())
                    );
                    send_control(
                        dialer.as_ref(),
                        &Control::SubscribeOk(SubscribeOk {
                            subscription_id: 2,
                            publisher_priority: u8::MAX,
                            publisher_max_latency_ms: DEFAULT_MAX_LATENCY_MS,
                            start_group: None,
                            end_group: None,
                        }),
                    )
                    .await
                    .expect("catalog subscription accepted");
                    let accepted = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .expect("catalog accept event in time")
                        .expect("catalog accept event");
                    assert!(matches!(
                        accepted.body,
                        EventBody::Control(Control::SubscribeOk(_))
                    ));
                    send_control(dialer.as_ref(), &Control::TrackInfo(catalog_track_info()))
                        .await
                        .expect("catalog track info");
                    let track = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .expect("catalog track event in time")
                        .expect("catalog track event");
                    assert!(matches!(
                        track.body,
                        EventBody::Control(Control::TrackInfo(TrackInfo {
                            kind: TrackKind::Catalog,
                            ..
                        }))
                    ));

                    let catalog_group = catalog_group(1);
                    let catalog_frame = catalog_group.frames.first().expect("one Frame");
                    let mut sending_catalog = OutgoingGroup::begin(
                        dialer.as_ref(),
                        catalog_group.header.clone(),
                        u8::MAX.into(),
                    )
                    .await
                    .expect("begin catalog");
                    sending_catalog
                        .write_frame(catalog_frame.header.clone(), &catalog_frame.payload)
                        .await
                        .expect("catalog Frame");
                    sending_catalog.finish().expect("finish catalog");
                    let catalog_event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .expect("catalog event in time")
                        .expect("catalog event");
                    let EventBody::Group(received_catalog) = catalog_event.body else {
                        panic!("catalog Group event");
                    };
                    assert_eq!(Catalog::from_group(&received_catalog), Ok(catalog()));

                    session
                        .send(Control::Subscribe(subscribe()))
                        .await
                        .expect("subscribe");
                    assert_eq!(
                        accept_media_control(dialer.as_ref()).await,
                        Control::Subscribe(subscribe())
                    );
                    // Deliberately make the uni Group visible before the bi
                    // acceptance records. Independent QUIC accept queues may
                    // present this order even when the publisher wrote OK
                    // first; the header waits without allocating its payload.
                    let payload = b"key-access-unit";
                    let mut group = OutgoingGroup::begin(dialer.as_ref(), group_header(17), 17)
                        .await
                        .expect("begin");
                    send_control(
                        dialer.as_ref(),
                        &Control::SubscribeOk(SubscribeOk {
                            subscription_id: 7,
                            publisher_priority: 17,
                            publisher_max_latency_ms: DEFAULT_MAX_LATENCY_MS,
                            start_group: None,
                            end_group: None,
                        }),
                    )
                    .await
                    .expect("subscription accepted");
                    let accepted = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .expect("accept event in time")
                        .expect("accept event");
                    assert!(matches!(
                        accepted.body,
                        EventBody::Control(Control::SubscribeOk(_))
                    ));
                    send_control(dialer.as_ref(), &Control::TrackInfo(track_info()))
                        .await
                        .expect("track info");
                    let track = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .expect("track event in time")
                        .expect("track event");
                    assert!(matches!(
                        track.body,
                        EventBody::Control(Control::TrackInfo(_))
                    ));

                    group
                        .write_frame(
                            FrameHeader {
                                timestamp: 90_000,
                                duration: Some(3_000),
                                timescale: 90_000,
                                kind: FrameKind::Key,
                                payload_len: u32::try_from(payload.len()).expect("small"),
                            },
                            payload,
                        )
                        .await
                        .expect("frame");
                    group.finish().expect("finish");
                    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .expect("event in time")
                        .expect("event");
                    cancel.cancel();
                    dialer.close(0, b"done");
                    event
                };
                let ((), event) = tokio::join!(serving, sending);
                assert_eq!(event.peer, station);
                assert_eq!(event.connection_id, [9u8; 16]);
                let EventBody::Group(group) = event.body else {
                    panic!("a Group event");
                };
                assert_eq!(group.header.group_sequence, 17);
                assert_eq!(group.frames[0].payload, b"key-access-unit");
            })
            .await;
    }
}
