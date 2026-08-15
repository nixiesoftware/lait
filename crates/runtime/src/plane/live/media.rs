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
/// One Group header or Frame header.
pub const MAX_MEDIA_HEADER_BYTES: usize = 4 * 1024;
/// One encoded access unit. Checked before its payload is allocated.
pub const MAX_MEDIA_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// One Group cannot acquire an unbounded frame table.
pub const MAX_FRAMES_PER_GROUP: usize = 512;
/// Aggregate receiver memory ceiling for one materialized Group.
pub const MAX_MEDIA_GROUP_BYTES: usize = 32 * 1024 * 1024;
/// Bounded event fan-out from the plane to local consumers.
const EVENT_QUEUE: usize = 8;
/// Reset/stop code for a malformed or expired media flow.
pub const RESET_MEDIA: u32 = 3;

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
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Invalid {}

/// The application meaning of a Track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
    Catalog,
}

/// Whether an encoded chunk can be decoded independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
                validate_codec(&value.codec)?;
                if value.timescale == 0
                    || value.decoder_config.len() > MAX_DECODER_CONFIG_BYTES
                    || value.max_group_duration_ms == 0
                    || value.max_group_duration_ms > MAX_GROUP_DURATION_MS
                {
                    return Err(Invalid::Bounds);
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

/// One validated media event, bound to the admitted peer and connection that
/// carried it.
#[derive(Debug, Clone)]
pub struct Event {
    pub peer: Key,
    pub connection_id: [u8; 16],
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
        if self.frames == 0 {
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
    let header_bytes = read_required_framed(flow, MAX_MEDIA_HEADER_BYTES).await?;
    let header = GroupHeader::decode_canonical(&header_bytes)?;
    let mut frames = Vec::new();
    let mut bytes = header_bytes.len();
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
    Ok(ReceivedGroup { header, frames })
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
        || codec.bytes().any(|byte| byte.is_ascii_control())
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
                    let payload = b"key-access-unit";
                    let mut group = OutgoingGroup::begin(dialer.as_ref(), group_header(17), 17)
                        .await
                        .expect("begin");
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
