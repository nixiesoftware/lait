//! Bounded CMAF packaging at the receiver edge.
//!
//! lait-live deliberately carries raw encoded access units. The display
//! coordinator is the first place that owns a browser-shaped container, so it
//! transmuxes one validated Track at a time into an ISO-BMFF initialization
//! segment and independent media fragments. It never decodes or re-encodes.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use mp4_atom::esds::{DecoderConfig, DecoderSpecific, EsDescriptor, SLConfig};
use mp4_atom::{
    Audio, Avc1, Avcc, Codec, Decode, Dinf, Dref, Encode, Esds, FourCC, Ftyp, Hdlr, Mdat, Mdhd,
    Mdia, Mfhd, Minf, Moof, Moov, Mp4a, Mvex, Mvhd, Smhd, Stbl, Stsd, Styp, Tfdt, Tfhd, Tkhd, Traf,
    Trak, Trex, Trun, TrunEntry, Url, Visual, Vmhd,
};
use runtime::plane::live::media::{
    Catalog, CatalogTrack, Frame, FrameKind, ReceivedGroup, TrackKind, MAX_FRAMES_PER_GROUP,
    MAX_MEDIA_GROUP_BYTES,
};

const TRACK_ID: u32 = 1;
const FIRST_FRAGMENT_SEQUENCE: u32 = 1;
const MAX_INIT_SEGMENT_BYTES: usize = 256 * 1024;
const MAX_FRAGMENT_OVERHEAD_BYTES: usize = 128 * 1024;
const VIDEO_KEY_SAMPLE_FLAGS: u32 = 0x0200_0000;
const VIDEO_DELTA_SAMPLE_FLAGS: u32 = 0x0101_0000;
const AUDIO_SAMPLE_FLAGS: u32 = 0x0200_0000;

/// A refused container operation. Inputs have already crossed the lait-live
/// bounds, but the receiver edge validates them again before allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    InvalidCatalog,
    MissingRendition,
    UnsupportedCodec,
    InvalidGroup,
    MissingDuration,
    TimestampOutOfRange,
    SequenceExhausted,
    TooLarge,
    Container,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidCatalog => "invalid live-media catalog track",
            Self::MissingRendition => "catalog track has no CMAF rendition",
            Self::UnsupportedCodec => "codec is not supported by the CMAF edge",
            Self::InvalidGroup => "live-media Group does not match its CMAF track",
            Self::MissingDuration => "encoded sample has no presentation duration",
            Self::TimestampOutOfRange => "encoded sample timestamp is outside the CMAF timeline",
            Self::SequenceExhausted => "CMAF fragment sequence is exhausted",
            Self::TooLarge => "CMAF output exceeds its receiver bound",
            Self::Container => "ISO-BMFF container encoding failed",
        };
        formatter.write_str(message)
    }
}

impl Error for Failure {}

/// One complete MSE media segment. A discontinuity tells the receiver to
/// discard its old SourceBuffer timeline and append the init segment again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmafFragment {
    pub group_sequence: u64,
    pub published_at_micros: i64,
    pub start_timestamp: u64,
    pub duration: u64,
    pub discontinuity: bool,
    pub bytes: Vec<u8>,
}

/// The closed receiver-facing description of one catalog Track. The opaque
/// rendition id is the only name an assignment route needs to expose; it is
/// intentionally not a path or a caller-provided URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmafTrackDescription {
    pub track: String,
    pub rendition: String,
    pub kind: TrackKind,
    pub mime_type: String,
    pub timescale: u32,
    pub target_latency_ms: u32,
    pub render_group: Option<String>,
    pub init_segment: Vec<u8>,
}

/// One routed fragment plus the opaque rendition selected by the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmafRenditionFragment {
    pub rendition: String,
    pub fragment: CmafFragment,
}

/// Catalog-bound packaging for all mandatory browser renditions in one live
/// presentation. Optional codecs that do not yet have a CMAF sample entry are
/// ignored while the mandatory H.264/AAC alternatives remain available.
pub struct CmafCatalogPackager {
    descriptions: Vec<CmafTrackDescription>,
    tracks: BTreeMap<String, CmafTrackPackager>,
}

impl CmafCatalogPackager {
    pub fn new(catalog: &Catalog) -> Result<Self, Failure> {
        catalog
            .encode_canonical()
            .map_err(|_| Failure::InvalidCatalog)?;
        let mut descriptions = Vec::new();
        let mut tracks = BTreeMap::new();
        for track in catalog
            .tracks
            .iter()
            .filter(|track| track.cmaf_rendition.is_some())
        {
            let packager = match CmafTrackPackager::new(track) {
                Ok(packager) => packager,
                Err(Failure::UnsupportedCodec) => continue,
                Err(error) => return Err(error),
            };
            descriptions.push(packager.description());
            if tracks.insert(track.track.clone(), packager).is_some() {
                return Err(Failure::InvalidCatalog);
            }
        }
        if tracks.is_empty() {
            return Err(Failure::MissingRendition);
        }
        Ok(Self {
            descriptions,
            tracks,
        })
    }

    pub fn descriptions(&self) -> &[CmafTrackDescription] {
        &self.descriptions
    }

    pub fn push_group(&mut self, group: &ReceivedGroup) -> Result<CmafRenditionFragment, Failure> {
        let packager = self
            .tracks
            .get_mut(&group.header.track)
            .ok_or(Failure::InvalidGroup)?;
        let rendition = packager.rendition.clone();
        let fragment = packager.push_group(group)?;
        Ok(CmafRenditionFragment {
            rendition,
            fragment,
        })
    }
}

/// Stateful packaging for one catalog Track/rendition.
pub struct CmafTrackPackager {
    track: String,
    rendition: String,
    kind: TrackKind,
    codec: String,
    timescale: u32,
    max_group_duration_ms: u32,
    target_latency_ms: u32,
    render_group: Option<String>,
    init_segment: Vec<u8>,
    next_fragment_sequence: u32,
    last_group_sequence: Option<u64>,
    last_end_timestamp: Option<u64>,
}

impl CmafTrackPackager {
    pub fn new(track: &CatalogTrack) -> Result<Self, Failure> {
        let track_info = track.track_info().map_err(|_| Failure::InvalidCatalog)?;
        let rendition = track
            .cmaf_rendition
            .clone()
            .ok_or(Failure::MissingRendition)?;
        let codec = codec_sample_entry(track)?;
        let init_segment = init_segment(track, codec)?;
        Ok(Self {
            track: track_info.track,
            rendition,
            kind: track_info.kind,
            codec: track_info.codec,
            timescale: track_info.timescale,
            max_group_duration_ms: track_info.max_group_duration_ms,
            target_latency_ms: track.target_latency_ms,
            render_group: track.render_group.clone(),
            init_segment,
            next_fragment_sequence: FIRST_FRAGMENT_SEQUENCE,
            last_group_sequence: None,
            last_end_timestamp: None,
        })
    }

    pub fn rendition(&self) -> &str {
        &self.rendition
    }

    pub fn mime_type(&self) -> String {
        let media = match self.kind {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
            TrackKind::Catalog => "application",
        };
        format!("{media}/mp4; codecs=\"{}\"", self.codec)
    }

    pub fn init_segment(&self) -> &[u8] {
        &self.init_segment
    }

    fn description(&self) -> CmafTrackDescription {
        CmafTrackDescription {
            track: self.track.clone(),
            rendition: self.rendition.clone(),
            kind: self.kind,
            mime_type: self.mime_type(),
            timescale: self.timescale,
            target_latency_ms: self.target_latency_ms,
            render_group: self.render_group.clone(),
            init_segment: self.init_segment.clone(),
        }
    }

    pub fn push_group(&mut self, group: &ReceivedGroup) -> Result<CmafFragment, Failure> {
        let samples = self.validate_group(group)?;
        let start_timestamp = samples
            .first()
            .map(|sample| sample.timestamp)
            .ok_or(Failure::InvalidGroup)?;
        let end_timestamp = samples
            .last()
            .and_then(|sample| sample.timestamp.checked_add(u64::from(sample.duration)))
            .ok_or(Failure::TimestampOutOfRange)?;
        let duration = end_timestamp
            .checked_sub(start_timestamp)
            .ok_or(Failure::TimestampOutOfRange)?;
        let discontinuity = match (self.last_group_sequence, self.last_end_timestamp) {
            (Some(last_group), Some(last_end)) => {
                last_group.checked_add(1) != Some(group.header.group_sequence)
                    || last_end != start_timestamp
            }
            (None, None) => false,
            _ => true,
        };
        let bytes = media_segment(
            self.kind,
            self.next_fragment_sequence,
            start_timestamp,
            &samples,
            &group.frames,
        )?;
        self.next_fragment_sequence = self
            .next_fragment_sequence
            .checked_add(1)
            .ok_or(Failure::SequenceExhausted)?;
        self.last_group_sequence = Some(group.header.group_sequence);
        self.last_end_timestamp = Some(end_timestamp);
        Ok(CmafFragment {
            group_sequence: group.header.group_sequence,
            published_at_micros: group.header.published_at_micros,
            start_timestamp,
            duration,
            discontinuity,
            bytes,
        })
    }

    fn validate_group(&self, group: &ReceivedGroup) -> Result<Vec<Sample>, Failure> {
        if group.header.track != self.track
            || group.header.track_kind != self.kind
            || group.header.timescale != self.timescale
            || group.header.max_group_duration_ms != self.max_group_duration_ms
            || group.frames.is_empty()
            || group.frames.len() > MAX_FRAMES_PER_GROUP
            || self
                .last_group_sequence
                .is_some_and(|last| group.header.group_sequence <= last)
        {
            return Err(Failure::InvalidGroup);
        }
        let mut total_bytes = 0usize;
        let mut expected_timestamp = None;
        let mut samples = Vec::with_capacity(group.frames.len());
        for (index, frame) in group.frames.iter().enumerate() {
            let payload_len =
                usize::try_from(frame.header.payload_len).map_err(|_| Failure::TooLarge)?;
            if frame.header.timescale != self.timescale
                || payload_len != frame.payload.len()
                || payload_len == 0
                || (index == 0 && frame.header.kind != FrameKind::Key)
            {
                return Err(Failure::InvalidGroup);
            }
            total_bytes = total_bytes
                .checked_add(payload_len)
                .ok_or(Failure::TooLarge)?;
            if total_bytes > MAX_MEDIA_GROUP_BYTES {
                return Err(Failure::TooLarge);
            }
            let timestamp =
                u64::try_from(frame.header.timestamp).map_err(|_| Failure::TimestampOutOfRange)?;
            if expected_timestamp.is_some_and(|expected| timestamp != expected) {
                return Err(Failure::InvalidGroup);
            }
            let duration = frame
                .header
                .duration
                .ok_or(Failure::MissingDuration)
                .and_then(|duration| {
                    u32::try_from(duration).map_err(|_| Failure::TimestampOutOfRange)
                })?;
            if duration == 0 {
                return Err(Failure::MissingDuration);
            }
            if self.kind == TrackKind::Video {
                validate_avc_sample(&frame.payload)?;
            }
            samples.push(Sample {
                timestamp,
                duration,
                size: frame.header.payload_len,
                kind: frame.header.kind,
            });
            expected_timestamp = Some(
                timestamp
                    .checked_add(u64::from(duration))
                    .ok_or(Failure::TimestampOutOfRange)?,
            );
        }
        let first = samples.first().ok_or(Failure::InvalidGroup)?;
        let last = samples.last().ok_or(Failure::InvalidGroup)?;
        let span = last
            .timestamp
            .checked_add(u64::from(last.duration))
            .and_then(|end| end.checked_sub(first.timestamp))
            .ok_or(Failure::TimestampOutOfRange)?;
        let maximum = u64::from(self.max_group_duration_ms)
            .checked_mul(u64::from(self.timescale))
            .and_then(|scaled| scaled.checked_div(1_000))
            .ok_or(Failure::TimestampOutOfRange)?;
        if span > maximum {
            return Err(Failure::InvalidGroup);
        }
        Ok(samples)
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    timestamp: u64,
    duration: u32,
    size: u32,
    kind: FrameKind,
}

fn codec_sample_entry(track: &CatalogTrack) -> Result<Codec, Failure> {
    let config = track
        .decoder_config()
        .map_err(|_| Failure::InvalidCatalog)?;
    match track.kind {
        TrackKind::Video if track.codec.starts_with("avc1.") => {
            let width = u16::try_from(track.width.ok_or(Failure::InvalidCatalog)?)
                .map_err(|_| Failure::InvalidCatalog)?;
            let height = u16::try_from(track.height.ok_or(Failure::InvalidCatalog)?)
                .map_err(|_| Failure::InvalidCatalog)?;
            let avcc = decode_avcc(&config)?;
            if avcc.length_size != 4 {
                return Err(Failure::InvalidCatalog);
            }
            Ok(Avc1 {
                visual: Visual {
                    data_reference_index: 1,
                    width,
                    height,
                    compressor: "Astrolabe H.264".into(),
                    ..Default::default()
                },
                avcc,
                ..Default::default()
            }
            .into())
        }
        TrackKind::Audio if matches!(track.codec.as_str(), "mp4a.40.2" | "mp4a.40.02") => {
            let channels = u16::from(track.channels.ok_or(Failure::InvalidCatalog)?);
            let sample_rate = track.sample_rate.ok_or(Failure::InvalidCatalog)?;
            let sample_rate = u16::try_from(sample_rate).map_err(|_| Failure::InvalidCatalog)?;
            Ok(Mp4a {
                audio: Audio {
                    data_reference_index: 1,
                    channel_count: channels,
                    sample_size: 16,
                    sample_rate: sample_rate.into(),
                },
                esds: Esds {
                    es_desc: EsDescriptor {
                        es_id: 1,
                        dec_config: DecoderConfig {
                            object_type_indication: 0x40,
                            stream_type: 0x05,
                            up_stream: 0,
                            buffer_size_db: Default::default(),
                            max_bitrate: track.bitrate_bps,
                            avg_bitrate: track.bitrate_bps,
                            dec_specific: Some(DecoderSpecific {
                                raw: config,
                                ..Default::default()
                            }),
                        },
                        sl_config: SLConfig::default(),
                    },
                },
                btrt: None,
                taic: None,
            }
            .into())
        }
        _ => Err(Failure::UnsupportedCodec),
    }
}

fn decode_avcc(description: &[u8]) -> Result<Avcc, Failure> {
    let size = description
        .len()
        .checked_add(8)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or(Failure::TooLarge)?;
    let mut atom = Vec::with_capacity(usize::try_from(size).map_err(|_| Failure::TooLarge)?);
    atom.extend_from_slice(&size.to_be_bytes());
    atom.extend_from_slice(b"avcC");
    atom.extend_from_slice(description);
    let mut input = atom.as_slice();
    Avcc::decode(&mut input).map_err(|_| Failure::InvalidCatalog)
}

fn validate_avc_sample(mut payload: &[u8]) -> Result<(), Failure> {
    while !payload.is_empty() {
        let length = payload.get(..4).ok_or(Failure::InvalidGroup)?;
        let length = u32::from_be_bytes(length.try_into().map_err(|_| Failure::InvalidGroup)?);
        let length = usize::try_from(length).map_err(|_| Failure::TooLarge)?;
        if length == 0 {
            return Err(Failure::InvalidGroup);
        }
        let consumed = 4usize.checked_add(length).ok_or(Failure::TooLarge)?;
        payload = payload.get(consumed..).ok_or(Failure::InvalidGroup)?;
    }
    Ok(())
}

fn init_segment(track: &CatalogTrack, codec: Codec) -> Result<Vec<u8>, Failure> {
    let (width, height, volume, handler, handler_name, vmhd, smhd) = match track.kind {
        TrackKind::Video => (
            track.width.ok_or(Failure::InvalidCatalog)?,
            track.height.ok_or(Failure::InvalidCatalog)?,
            0,
            FourCC::new(b"vide"),
            "VideoHandler",
            Some(Vmhd::default()),
            None,
        ),
        TrackKind::Audio => (
            0,
            0,
            1,
            FourCC::new(b"soun"),
            "SoundHandler",
            None,
            Some(Smhd::default()),
        ),
        TrackKind::Catalog => return Err(Failure::UnsupportedCodec),
    };
    let ftyp = Ftyp {
        major_brand: FourCC::new(b"iso6"),
        minor_version: 1,
        compatible_brands: vec![
            FourCC::new(b"iso6"),
            FourCC::new(b"cmfc"),
            FourCC::new(b"mp41"),
        ],
    };
    let moov = Moov {
        mvhd: Mvhd {
            timescale: track.timescale,
            rate: 1.into(),
            volume: 1.into(),
            next_track_id: TRACK_ID + 1,
            ..Default::default()
        },
        mvex: Some(Mvex {
            mehd: None,
            trex: vec![Trex {
                track_id: TRACK_ID,
                default_sample_description_index: 1,
                ..Default::default()
            }],
        }),
        trak: vec![Trak {
            tkhd: Tkhd {
                track_id: TRACK_ID,
                enabled: true,
                in_movie: true,
                volume: volume.into(),
                width: u16::try_from(width)
                    .map_err(|_| Failure::InvalidCatalog)?
                    .into(),
                height: u16::try_from(height)
                    .map_err(|_| Failure::InvalidCatalog)?
                    .into(),
                ..Default::default()
            },
            mdia: Mdia {
                mdhd: Mdhd {
                    timescale: track.timescale,
                    language: "und".into(),
                    ..Default::default()
                },
                hdlr: Hdlr {
                    handler,
                    name: handler_name.into(),
                },
                minf: Minf {
                    vmhd,
                    smhd,
                    dinf: Dinf {
                        dref: Dref {
                            urls: vec![Url::default()],
                        },
                    },
                    stbl: Stbl {
                        stsd: Stsd {
                            codecs: vec![codec],
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut bytes = Vec::new();
    ftyp.encode(&mut bytes).map_err(container_error)?;
    moov.encode(&mut bytes).map_err(container_error)?;
    if bytes.len() > MAX_INIT_SEGMENT_BYTES {
        return Err(Failure::TooLarge);
    }
    Ok(bytes)
}

fn media_segment(
    kind: TrackKind,
    sequence: u32,
    start_timestamp: u64,
    samples: &[Sample],
    frames: &[Frame],
) -> Result<Vec<u8>, Failure> {
    let entries = samples
        .iter()
        .map(|sample| TrunEntry {
            duration: Some(sample.duration),
            size: Some(sample.size),
            flags: Some(sample_flags(kind, sample.kind)),
            cts: None,
        })
        .collect();
    let mut moof = Moof {
        mfhd: Mfhd {
            sequence_number: sequence,
        },
        traf: vec![Traf {
            tfhd: Tfhd {
                track_id: TRACK_ID,
                default_base_is_moof: true,
                ..Default::default()
            },
            tfdt: Some(Tfdt {
                base_media_decode_time: start_timestamp,
            }),
            trun: vec![Trun {
                data_offset: Some(0),
                entries,
            }],
            ..Default::default()
        }],
    };
    let mut moof_bytes = Vec::new();
    moof.encode(&mut moof_bytes).map_err(container_error)?;
    let data_offset = moof_bytes
        .len()
        .checked_add(8)
        .and_then(|offset| i32::try_from(offset).ok())
        .ok_or(Failure::TooLarge)?;
    let trun = moof
        .traf
        .first_mut()
        .and_then(|traf| traf.trun.first_mut())
        .ok_or(Failure::Container)?;
    trun.data_offset = Some(data_offset);
    moof_bytes.clear();
    moof.encode(&mut moof_bytes).map_err(container_error)?;

    let payload_len = frames.iter().try_fold(0usize, |total, frame| {
        total
            .checked_add(frame.payload.len())
            .ok_or(Failure::TooLarge)
    })?;
    let mut payload = Vec::with_capacity(payload_len);
    for frame in frames {
        payload.extend_from_slice(&frame.payload);
    }
    let styp = Styp {
        major_brand: FourCC::new(b"cmfs"),
        minor_version: 0,
        compatible_brands: vec![FourCC::new(b"cmfs"), FourCC::new(b"iso6")],
    };
    let mdat = Mdat { data: payload };
    let maximum = MAX_MEDIA_GROUP_BYTES
        .checked_add(MAX_FRAGMENT_OVERHEAD_BYTES)
        .ok_or(Failure::TooLarge)?;
    let mut bytes = Vec::with_capacity(
        payload_len
            .checked_add(MAX_FRAGMENT_OVERHEAD_BYTES)
            .ok_or(Failure::TooLarge)?,
    );
    styp.encode(&mut bytes).map_err(container_error)?;
    bytes.extend_from_slice(&moof_bytes);
    mdat.encode(&mut bytes).map_err(container_error)?;
    if bytes.len() > maximum {
        return Err(Failure::TooLarge);
    }
    Ok(bytes)
}

fn sample_flags(kind: TrackKind, frame: FrameKind) -> u32 {
    match (kind, frame) {
        (TrackKind::Video, FrameKind::Key) => VIDEO_KEY_SAMPLE_FLAGS,
        (TrackKind::Video, FrameKind::Delta) => VIDEO_DELTA_SAMPLE_FLAGS,
        (TrackKind::Audio | TrackKind::Catalog, _) => AUDIO_SAMPLE_FLAGS,
    }
}

fn container_error(_: mp4_atom::Error) -> Failure {
    Failure::Container
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp4_atom::{Any, Atom, Buf, DecodeAtom, DecodeMaybe, Header};
    use runtime::plane::live::media::{FrameHeader, GroupHeader, CATALOG_VERSION};

    fn video_track() -> CatalogTrack {
        CatalogTrack {
            track: "screen/main".into(),
            kind: TrackKind::Video,
            codec: "avc1.640028".into(),
            timescale: 90_000,
            decoder_config_hex: "01640028ffe100046764002801000268eb".into(),
            max_group_duration_ms: 2_000,
            target_latency_ms: 2_000,
            bitrate_bps: 4_000_000,
            width: Some(1_920),
            height: Some(1_080),
            frame_rate_milli: Some(30_000),
            sample_rate: None,
            channels: None,
            render_group: Some("main".into()),
            cmaf_rendition: Some("main_h264".into()),
            hls_v3_rendition: Some("main_h264".into()),
        }
    }

    fn video_group(sequence: u64, timestamp: i64) -> ReceivedGroup {
        ReceivedGroup {
            header: GroupHeader {
                subscription_id: 7,
                track: "screen/main".into(),
                track_kind: TrackKind::Video,
                group_sequence: sequence,
                published_at_micros: 50,
                timescale: 90_000,
                max_group_duration_ms: 2_000,
            },
            frames: vec![
                frame(timestamp, FrameKind::Key, &[0, 0, 0, 1, 0x65]),
                frame(timestamp + 3_000, FrameKind::Delta, &[0, 0, 0, 1, 0x41]),
            ],
        }
    }

    fn audio_track() -> CatalogTrack {
        CatalogTrack {
            track: "audio/main".into(),
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
        }
    }

    fn audio_group(sequence: u64, timestamp: i64) -> ReceivedGroup {
        ReceivedGroup {
            header: GroupHeader {
                subscription_id: 8,
                track: "audio/main".into(),
                track_kind: TrackKind::Audio,
                group_sequence: sequence,
                published_at_micros: 50,
                timescale: 48_000,
                max_group_duration_ms: 2_000,
            },
            frames: vec![
                timed_frame(timestamp, 1_024, 48_000, FrameKind::Key, &[0x21, 0x10]),
                timed_frame(
                    timestamp + 1_024,
                    1_024,
                    48_000,
                    FrameKind::Key,
                    &[0x22, 0x20],
                ),
            ],
        }
    }

    fn frame(timestamp: i64, kind: FrameKind, payload: &[u8]) -> Frame {
        timed_frame(timestamp, 3_000, 90_000, kind, payload)
    }

    fn timed_frame(
        timestamp: i64,
        duration: u64,
        timescale: u32,
        kind: FrameKind,
        payload: &[u8],
    ) -> Frame {
        Frame {
            header: FrameHeader {
                timestamp,
                duration: Some(duration),
                timescale,
                kind,
                payload_len: u32::try_from(payload.len()).expect("bounded fixture"),
            },
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn init_segment_is_mse_shaped_and_carries_the_decoder_config() {
        let packager = CmafTrackPackager::new(&video_track()).expect("CMAF track");
        assert_eq!(packager.rendition(), "main_h264");
        assert_eq!(packager.mime_type(), "video/mp4; codecs=\"avc1.640028\"");

        let mut bytes = packager.init_segment();
        let ftyp = Ftyp::decode(&mut bytes).expect("ftyp");
        let moov = Moov::decode(&mut bytes).expect("moov");
        assert!(!bytes.has_remaining());
        assert_eq!(ftyp.major_brand, FourCC::new(b"iso6"));
        assert!(moov.mvex.is_some());
        let avc1 = moov
            .trak
            .first()
            .and_then(|trak| trak.mdia.minf.stbl.stsd.codecs.first())
            .and_then(|codec| match codec {
                Codec::Avc1(avc1) => Some(avc1),
                _ => None,
            })
            .expect("avc1 sample entry");
        assert_eq!(avc1.visual.width, 1_920);
        assert_eq!(avc1.visual.height, 1_080);
        assert_eq!(
            avc1.avcc.sequence_parameter_sets,
            vec![vec![0x67, 0x64, 0, 0x28]]
        );
        assert_eq!(avc1.avcc.picture_parameter_sets, vec![vec![0x68, 0xeb]]);
    }

    #[test]
    fn media_segment_is_styp_moof_mdat_with_bounded_sample_metadata() {
        let mut packager = CmafTrackPackager::new(&video_track()).expect("CMAF track");
        let group = video_group(9, 90_000);
        let fragment = packager.push_group(&group).expect("media fragment");
        assert_eq!(fragment.group_sequence, 9);
        assert_eq!(fragment.published_at_micros, 50);
        assert_eq!(fragment.start_timestamp, 90_000);
        assert_eq!(fragment.duration, 6_000);
        assert!(!fragment.discontinuity);

        let mut bytes = fragment.bytes.as_slice();
        let _styp = Styp::decode(&mut bytes).expect("styp");
        let moof_start = bytes.len();
        let moof = Moof::decode(&mut bytes).expect("moof");
        let moof_size = moof_start - bytes.len();
        let mdat = Mdat::decode(&mut bytes).expect("mdat");
        assert!(!bytes.has_remaining());
        assert_eq!(
            mdat.data,
            [
                group.frames[0].payload.clone(),
                group.frames[1].payload.clone()
            ]
            .concat()
        );
        let trun = &moof.traf[0].trun[0];
        assert_eq!(
            trun.data_offset,
            Some(i32::try_from(moof_size + 8).expect("small moof"))
        );
        assert_eq!(trun.entries.len(), 2);
        assert_eq!(trun.entries[0].flags, Some(VIDEO_KEY_SAMPLE_FLAGS));
        assert_eq!(trun.entries[1].flags, Some(VIDEO_DELTA_SAMPLE_FLAGS));
    }

    #[test]
    fn aac_init_and_media_segments_preserve_audio_configuration() {
        let mut packager = CmafTrackPackager::new(&audio_track()).expect("AAC CMAF track");
        assert_eq!(packager.mime_type(), "audio/mp4; codecs=\"mp4a.40.2\"");

        let mut init = packager.init_segment();
        let _ftyp = Ftyp::decode(&mut init).expect("ftyp");
        let moov = Moov::decode(&mut init).expect("moov");
        assert!(!init.has_remaining());
        let mp4a = moov
            .trak
            .first()
            .and_then(|trak| trak.mdia.minf.stbl.stsd.codecs.first())
            .and_then(|codec| match codec {
                Codec::Mp4a(mp4a) => Some(mp4a),
                _ => None,
            })
            .expect("mp4a sample entry");
        assert_eq!(mp4a.audio.channel_count, 2);
        assert_eq!(mp4a.audio.sample_rate, 48_000.into());
        assert_eq!(
            mp4a.esds
                .es_desc
                .dec_config
                .dec_specific
                .as_ref()
                .map(|specific| specific.raw.as_slice()),
            Some([0x11, 0x90].as_slice())
        );

        let fragment = packager
            .push_group(&audio_group(1, 0))
            .expect("AAC media fragment");
        assert_eq!(fragment.duration, 2_048);
        let mut bytes = fragment.bytes.as_slice();
        let _styp = Styp::decode(&mut bytes).expect("styp");
        let moof = Moof::decode(&mut bytes).expect("moof");
        let mdat = Mdat::decode(&mut bytes).expect("mdat");
        assert!(!bytes.has_remaining());
        assert_eq!(moof.traf[0].trun[0].entries.len(), 2);
        assert_eq!(mdat.data, vec![0x21, 0x10, 0x22, 0x20]);
    }

    #[test]
    fn catalog_packager_exposes_closed_renditions_and_routes_groups() {
        let catalog = Catalog {
            version: CATALOG_VERSION,
            jitter_hint_ms: 250,
            tracks: vec![video_track(), audio_track()],
        };
        let mut packager = CmafCatalogPackager::new(&catalog).expect("catalog packager");
        assert_eq!(packager.descriptions().len(), 2);
        assert!(packager.descriptions().iter().all(|track| {
            !track.track.is_empty()
                && !track.rendition.is_empty()
                && !track.init_segment.is_empty()
                && track.mime_type.contains("/mp4")
                && track.target_latency_ms == 2_000
                && track.render_group.as_deref() == Some("main")
        }));

        let output = packager
            .push_group(&video_group(1, 0))
            .expect("routed video group");
        assert_eq!(output.rendition, "main_h264");
        assert_eq!(output.fragment.group_sequence, 1);
        let output = packager
            .push_group(&audio_group(1, 0))
            .expect("routed audio group");
        assert_eq!(output.rendition, "main_aac");
    }

    #[test]
    fn sequence_or_timeline_gaps_are_explicit_discontinuities() {
        let mut packager = CmafTrackPackager::new(&video_track()).expect("CMAF track");
        packager
            .push_group(&video_group(1, 0))
            .expect("first fragment");
        let fragment = packager
            .push_group(&video_group(3, 12_000))
            .expect("newest fragment after drop");
        assert!(fragment.discontinuity);
    }

    #[test]
    fn incomplete_or_replayed_groups_fail_closed() {
        let mut packager = CmafTrackPackager::new(&video_track()).expect("CMAF track");
        let mut missing_duration = video_group(1, 0);
        missing_duration.frames[0].header.duration = None;
        assert_eq!(
            packager.push_group(&missing_duration),
            Err(Failure::MissingDuration)
        );
        packager
            .push_group(&video_group(1, 0))
            .expect("first complete group");
        assert_eq!(
            packager.push_group(&video_group(1, 6_000)),
            Err(Failure::InvalidGroup)
        );

        let mut packager = CmafTrackPackager::new(&video_track()).expect("CMAF track");
        let mut timestamp_gap = video_group(1, 0);
        timestamp_gap.frames[1].header.timestamp += 1;
        assert_eq!(
            packager.push_group(&timestamp_gap),
            Err(Failure::InvalidGroup)
        );

        let mut malformed_avc = video_group(1, 0);
        malformed_avc.frames[0].payload.push(0x88);
        malformed_avc.frames[0].header.payload_len += 1;
        assert_eq!(
            packager.push_group(&malformed_avc),
            Err(Failure::InvalidGroup)
        );
    }

    #[test]
    fn generated_segments_are_only_the_expected_top_level_boxes() {
        let mut packager = CmafTrackPackager::new(&video_track()).expect("CMAF track");
        let fragment = packager
            .push_group(&video_group(1, 0))
            .expect("media fragment");
        let mut bytes = fragment.bytes.as_slice();
        let mut kinds = Vec::new();
        while let Some(header) = Header::decode_maybe(&mut bytes).expect("box header") {
            kinds.push(header.kind);
            let size = header.size.expect("bounded box");
            let _ = Any::decode_atom(&header, &mut bytes.slice(size)).expect("known box");
            bytes.advance(size);
        }
        assert_eq!(kinds, vec![Styp::KIND, Moof::KIND, Mdat::KIND],);
    }
}
