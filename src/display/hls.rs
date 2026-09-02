//! Classic HLS v3 packaging for native television receivers.
//!
//! HLS protocol version three predates fragmented MP4. These segments are
//! actual MPEG-2 Transport Streams with PAT, PMT, PCR, and PES framing. The
//! coordinator transmuxes only: encoded H.264/AAC access units are never
//! decoded or re-encoded.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use libmpegts::mux::{Multiplexer, MuxFrame, MuxService, MuxStream};
use runtime::plane::live::media::{
    Catalog, CatalogTrack, FrameKind, ReceivedGroup, TrackKind, MAX_MEDIA_GROUP_BYTES,
};

const VIDEO_PID: u16 = 0x101;
const AUDIO_PID: u16 = 0x102;
const PMT_PID: u16 = 0x100;
const TS_PACKET_BYTES: usize = 188;
const DRAIN_PACKETS: usize = 512;
const MAX_HLS_SEGMENT_BYTES: usize = MAX_MEDIA_GROUP_BYTES * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    InvalidCatalog,
    MissingRendition,
    UnsupportedCodec,
    InvalidGroup,
    TimestampOutOfRange,
    TooLarge,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCatalog => "invalid live-media catalog for HLS",
            Self::MissingRendition => "catalog has no HLS v3 rendition",
            Self::UnsupportedCodec => "codec is not supported by HLS v3",
            Self::InvalidGroup => "live-media Group does not match its HLS rendition",
            Self::TimestampOutOfRange => "media timestamp is outside the MPEG-TS clock",
            Self::TooLarge => "HLS segment exceeds its receiver bound",
        })
    }
}

impl std::error::Error for Failure {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsRenditionDescription {
    pub rendition: String,
    pub target_duration_ms: u32,
    pub codecs: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bitrate_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsSegment {
    pub rendition: String,
    pub group_sequence: u64,
    pub published_at_micros: i64,
    pub duration_ms: u32,
    pub discontinuity: bool,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
struct Track {
    catalog: CatalogTrack,
    avc_parameter_sets: Vec<u8>,
    aac: Option<AacConfig>,
}

#[derive(Clone, Copy)]
struct AacConfig {
    profile: u8,
    sampling_frequency_index: u8,
    channel_configuration: u8,
}

struct Rendition {
    tracks: BTreeSet<String>,
    pending: BTreeMap<u64, BTreeMap<String, ReceivedGroup>>,
    last_sequence: Option<u64>,
}

/// Catalog-bound, bounded MPEG-TS segmenter. Tracks that share an opaque HLS
/// rendition are interleaved into one transport stream once their matching
/// Groups have all arrived.
pub struct HlsCatalogPackager {
    descriptions: Vec<HlsRenditionDescription>,
    tracks: BTreeMap<String, Track>,
    rendition_for_track: BTreeMap<String, String>,
    renditions: BTreeMap<String, Rendition>,
}

impl HlsCatalogPackager {
    pub fn new(catalog: &Catalog) -> Result<Self, Failure> {
        catalog
            .encode_canonical()
            .map_err(|_| Failure::InvalidCatalog)?;
        let mut tracks = BTreeMap::new();
        let mut rendition_for_track = BTreeMap::new();
        let mut renditions = BTreeMap::<String, Rendition>::new();
        for catalog_track in catalog
            .tracks
            .iter()
            .filter(|track| track.hls_v3_rendition.is_some())
        {
            let rendition = catalog_track
                .hls_v3_rendition
                .clone()
                .ok_or(Failure::MissingRendition)?;
            let track = Track::new(catalog_track.clone())?;
            if tracks.insert(catalog_track.track.clone(), track).is_some()
                || rendition_for_track
                    .insert(catalog_track.track.clone(), rendition.clone())
                    .is_some()
            {
                return Err(Failure::InvalidCatalog);
            }
            renditions
                .entry(rendition)
                .or_insert_with(|| Rendition {
                    tracks: BTreeSet::new(),
                    pending: BTreeMap::new(),
                    last_sequence: None,
                })
                .tracks
                .insert(catalog_track.track.clone());
        }
        if renditions.is_empty() {
            return Err(Failure::MissingRendition);
        }
        let descriptions = renditions
            .iter()
            .map(|(name, rendition)| description(name, rendition, &tracks))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            descriptions,
            tracks,
            rendition_for_track,
            renditions,
        })
    }

    pub fn descriptions(&self) -> &[HlsRenditionDescription] {
        &self.descriptions
    }

    /// Add one source Group. `Ok(None)` means the other synchronized tracks
    /// for this segment have not arrived yet.
    pub fn push_group(&mut self, group: &ReceivedGroup) -> Result<Option<HlsSegment>, Failure> {
        let track = self
            .tracks
            .get(&group.header.track)
            .ok_or(Failure::InvalidGroup)?;
        track.validate(group)?;
        let rendition_name = self
            .rendition_for_track
            .get(&group.header.track)
            .cloned()
            .ok_or(Failure::InvalidGroup)?;
        let rendition = self
            .renditions
            .get_mut(&rendition_name)
            .ok_or(Failure::InvalidGroup)?;
        if rendition
            .last_sequence
            .is_some_and(|last| group.header.group_sequence <= last)
        {
            return Err(Failure::InvalidGroup);
        }
        let sequence = group.header.group_sequence;
        let pending = rendition.pending.entry(sequence).or_default();
        if pending
            .insert(group.header.track.clone(), group.clone())
            .is_some()
        {
            return Err(Failure::InvalidGroup);
        }
        // Newest wins. Incomplete older Groups cannot become useful after the
        // live edge has advanced, and retaining them would turn skew into an
        // unbounded reassembly table.
        rendition
            .pending
            .retain(|candidate, _| *candidate >= sequence);
        let ready = rendition
            .pending
            .get(&sequence)
            .is_some_and(|groups| groups.keys().eq(rendition.tracks.iter()));
        if !ready {
            return Ok(None);
        }
        let groups = rendition
            .pending
            .remove(&sequence)
            .ok_or(Failure::InvalidGroup)?;
        let discontinuity = rendition
            .last_sequence
            .is_some_and(|last| last.checked_add(1) != Some(sequence));
        let segment = mux_segment(
            &rendition_name,
            sequence,
            discontinuity,
            &groups,
            &self.tracks,
        )?;
        rendition.last_sequence = Some(sequence);
        Ok(Some(segment))
    }
}

impl Track {
    fn new(catalog: CatalogTrack) -> Result<Self, Failure> {
        let config = catalog
            .decoder_config()
            .map_err(|_| Failure::InvalidCatalog)?;
        let (avc_parameter_sets, aac) = match catalog.kind {
            TrackKind::Video if catalog.codec.starts_with("avc1.") => {
                (avc_parameter_sets(&config)?, None)
            }
            TrackKind::Audio if matches!(catalog.codec.as_str(), "mp4a.40.2" | "mp4a.40.02") => {
                (Vec::new(), Some(aac_config(&config)?))
            }
            _ => return Err(Failure::UnsupportedCodec),
        };
        Ok(Self {
            catalog,
            avc_parameter_sets,
            aac,
        })
    }

    fn validate(&self, group: &ReceivedGroup) -> Result<(), Failure> {
        if group.header.track != self.catalog.track
            || group.header.track_kind != self.catalog.kind
            || group.header.timescale != self.catalog.timescale
            || group.header.max_group_duration_ms != self.catalog.max_group_duration_ms
            || group.frames.is_empty()
            || group.frames.first().map(|frame| frame.header.kind) != Some(FrameKind::Key)
        {
            return Err(Failure::InvalidGroup);
        }
        let mut bytes = 0usize;
        for frame in &group.frames {
            if frame.header.timescale != self.catalog.timescale
                || usize::try_from(frame.header.payload_len).ok() != Some(frame.payload.len())
                || frame.header.duration.is_none()
            {
                return Err(Failure::InvalidGroup);
            }
            bytes = bytes
                .checked_add(frame.payload.len())
                .ok_or(Failure::TooLarge)?;
        }
        if bytes > MAX_MEDIA_GROUP_BYTES {
            return Err(Failure::TooLarge);
        }
        Ok(())
    }
}

fn description(
    name: &str,
    rendition: &Rendition,
    tracks: &BTreeMap<String, Track>,
) -> Result<HlsRenditionDescription, Failure> {
    let selected = rendition
        .tracks
        .iter()
        .map(|name| tracks.get(name).ok_or(Failure::InvalidCatalog))
        .collect::<Result<Vec<_>, _>>()?;
    let target_duration_ms = selected
        .iter()
        .map(|track| track.catalog.max_group_duration_ms)
        .max()
        .ok_or(Failure::InvalidCatalog)?;
    let bitrate_bps = selected.iter().try_fold(0u32, |total, track| {
        total
            .checked_add(track.catalog.bitrate_bps)
            .ok_or(Failure::TooLarge)
    })?;
    let video = selected
        .iter()
        .find(|track| track.catalog.kind == TrackKind::Video);
    Ok(HlsRenditionDescription {
        rendition: name.to_string(),
        target_duration_ms,
        codecs: selected
            .iter()
            .map(|track| track.catalog.codec.clone())
            .collect(),
        width: video.and_then(|track| track.catalog.width),
        height: video.and_then(|track| track.catalog.height),
        bitrate_bps,
    })
}

fn mux_segment(
    rendition: &str,
    sequence: u64,
    discontinuity: bool,
    groups: &BTreeMap<String, ReceivedGroup>,
    tracks: &BTreeMap<String, Track>,
) -> Result<HlsSegment, Failure> {
    let mut streams = Vec::new();
    let has_video = groups
        .values()
        .any(|group| group.header.track_kind == TrackKind::Video);
    if has_video {
        streams.push(MuxStream {
            stream_type: 0x1b,
            elementary_pid: VIDEO_PID,
            stream_descriptors: Vec::new(),
        });
    }
    if groups
        .values()
        .any(|group| group.header.track_kind == TrackKind::Audio)
    {
        streams.push(MuxStream {
            stream_type: 0x0f,
            elementary_pid: AUDIO_PID,
            stream_descriptors: Vec::new(),
        });
    }
    let pcr_pid = if has_video { VIDEO_PID } else { AUDIO_PID };
    let mut mux = Multiplexer::new(1);
    mux.add_service(&MuxService {
        program_number: 1,
        pmt_pid: PMT_PID,
        pcr_pid,
        program_descriptors: Vec::new(),
        service_descriptors: Vec::new(),
        streams,
    });
    let mut earliest = None;
    let mut latest = None;
    let mut published_at_micros = i64::MIN;
    for (name, group) in groups {
        let track = tracks.get(name).ok_or(Failure::InvalidGroup)?;
        published_at_micros = published_at_micros.max(group.header.published_at_micros);
        let pid = match track.catalog.kind {
            TrackKind::Video => VIDEO_PID,
            TrackKind::Audio => AUDIO_PID,
            TrackKind::Catalog => return Err(Failure::InvalidGroup),
        };
        let stream = mux.stream_index(pid).ok_or(Failure::InvalidGroup)?;
        for frame in &group.frames {
            // Frames arrive in decode order; the PES carries both clocks
            // whenever presentation trails decode.
            let dts = clock_90k(frame.header.timestamp, track.catalog.timescale)?;
            let pts = clock_90k(
                frame
                    .header
                    .timestamp
                    .checked_add(i64::from(frame.header.composition_offset))
                    .ok_or(Failure::TimestampOutOfRange)?,
                track.catalog.timescale,
            )?;
            let duration = frame.header.duration.ok_or(Failure::InvalidGroup)?;
            let end = frame
                .header
                .timestamp
                .checked_add(i64::try_from(duration).map_err(|_| Failure::TimestampOutOfRange)?)
                .ok_or(Failure::TimestampOutOfRange)?;
            let end = clock_90k(end, track.catalog.timescale)?;
            earliest = Some(earliest.map_or(pts, |current: u64| current.min(pts)));
            latest = Some(latest.map_or(end, |current: u64| current.max(end)));
            let data = match track.catalog.kind {
                TrackKind::Video => annex_b(
                    frame.payload.as_slice(),
                    &track.avc_parameter_sets,
                    frame.header.kind,
                )?,
                TrackKind::Audio => adts(
                    frame.payload.as_slice(),
                    track.aac.ok_or(Failure::InvalidCatalog)?,
                )?,
                TrackKind::Catalog => return Err(Failure::InvalidGroup),
            };
            mux.push_frame(
                stream,
                MuxFrame {
                    data,
                    is_key_frame: frame.header.kind == FrameKind::Key,
                    pts_dts: Some((pts, (pts != dts).then_some(dts)).into()),
                },
            );
        }
    }
    let mut bytes = Vec::new();
    let mut buffer = vec![0u8; TS_PACKET_BYTES * DRAIN_PACKETS];
    loop {
        let written = mux.drain(&mut buffer);
        if written == 0 {
            break;
        }
        let next = bytes.len().checked_add(written).ok_or(Failure::TooLarge)?;
        if next > MAX_HLS_SEGMENT_BYTES {
            return Err(Failure::TooLarge);
        }
        bytes.extend_from_slice(buffer.get(..written).ok_or(Failure::TooLarge)?);
    }
    if bytes.is_empty() || bytes.len() % TS_PACKET_BYTES != 0 {
        return Err(Failure::InvalidGroup);
    }
    let duration_90k = latest
        .zip(earliest)
        .and_then(|(end, start)| end.checked_sub(start))
        .ok_or(Failure::TimestampOutOfRange)?;
    let duration_ms = duration_90k
        .checked_add(89)
        .and_then(|value| value.checked_div(90))
        .and_then(|value| u32::try_from(value).ok())
        .filter(|duration| *duration > 0)
        .ok_or(Failure::TimestampOutOfRange)?;
    Ok(HlsSegment {
        rendition: rendition.to_string(),
        group_sequence: sequence,
        published_at_micros,
        duration_ms,
        discontinuity,
        bytes,
    })
}

fn clock_90k(timestamp: i64, timescale: u32) -> Result<u64, Failure> {
    let timestamp = u128::try_from(timestamp).map_err(|_| Failure::TimestampOutOfRange)?;
    timestamp
        .checked_mul(90_000)
        .and_then(|value| value.checked_div(u128::from(timescale)))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(Failure::TimestampOutOfRange)
}

fn avc_parameter_sets(config: &[u8]) -> Result<Vec<u8>, Failure> {
    if config.len() < 7
        || config.first().copied() != Some(1)
        || config.get(4).copied().map(|value| value & 0x03) != Some(3)
    {
        return Err(Failure::InvalidCatalog);
    }
    let mut cursor = 6usize;
    let sps_count = config
        .get(5)
        .copied()
        .map(|value| usize::from(value & 0x1f))
        .ok_or(Failure::InvalidCatalog)?;
    let mut output = Vec::new();
    for _ in 0..sps_count {
        copy_parameter_set(config, &mut cursor, &mut output)?;
    }
    let pps_count = usize::from(*config.get(cursor).ok_or(Failure::InvalidCatalog)?);
    cursor = cursor.checked_add(1).ok_or(Failure::TooLarge)?;
    for _ in 0..pps_count {
        copy_parameter_set(config, &mut cursor, &mut output)?;
    }
    // A High-family profile (100, 110, 122, 144) carries an extension after
    // the picture parameter sets: chroma format, the two bit depths, and a
    // count of SPS extensions. They are walked so the box is proven whole,
    // and not emitted: an SPS extension is not needed to decode the stream.
    let profile = config.get(1).copied().unwrap_or(0);
    if matches!(profile, 100 | 110 | 122 | 144) && cursor < config.len() {
        cursor = cursor.checked_add(3).ok_or(Failure::InvalidCatalog)?;
        let ext_count = usize::from(*config.get(cursor).ok_or(Failure::InvalidCatalog)?);
        cursor = cursor.checked_add(1).ok_or(Failure::TooLarge)?;
        let mut discarded = Vec::new();
        for _ in 0..ext_count {
            copy_parameter_set(config, &mut cursor, &mut discarded)?;
        }
    }
    if output.is_empty() || cursor != config.len() {
        return Err(Failure::InvalidCatalog);
    }
    Ok(output)
}

fn copy_parameter_set(
    config: &[u8],
    cursor: &mut usize,
    output: &mut Vec<u8>,
) -> Result<(), Failure> {
    let length = config
        .get(*cursor..cursor.saturating_add(2))
        .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
        .map(u16::from_be_bytes)
        .map(usize::from)
        .ok_or(Failure::InvalidCatalog)?;
    *cursor = cursor.checked_add(2).ok_or(Failure::TooLarge)?;
    let end = cursor.checked_add(length).ok_or(Failure::TooLarge)?;
    let parameter_set = config.get(*cursor..end).ok_or(Failure::InvalidCatalog)?;
    if parameter_set.is_empty() {
        return Err(Failure::InvalidCatalog);
    }
    output.extend_from_slice(&[0, 0, 0, 1]);
    output.extend_from_slice(parameter_set);
    *cursor = end;
    Ok(())
}

fn annex_b(payload: &[u8], parameter_sets: &[u8], kind: FrameKind) -> Result<Vec<u8>, Failure> {
    let mut output = Vec::with_capacity(
        payload
            .len()
            .checked_add(parameter_sets.len())
            .ok_or(Failure::TooLarge)?,
    );
    if kind == FrameKind::Key {
        output.extend_from_slice(parameter_sets);
    }
    let mut cursor = 0usize;
    while cursor < payload.len() {
        let length_end = cursor.checked_add(4).ok_or(Failure::TooLarge)?;
        let length = payload
            .get(cursor..length_end)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
            .and_then(|length| usize::try_from(length).ok())
            .filter(|length| *length > 0)
            .ok_or(Failure::InvalidGroup)?;
        let end = length_end.checked_add(length).ok_or(Failure::TooLarge)?;
        let nal = payload.get(length_end..end).ok_or(Failure::InvalidGroup)?;
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(nal);
        cursor = end;
    }
    Ok(output)
}

fn aac_config(config: &[u8]) -> Result<AacConfig, Failure> {
    let bytes = config.get(..2).ok_or(Failure::InvalidCatalog)?;
    let bits = u16::from_be_bytes(<[u8; 2]>::try_from(bytes).map_err(|_| Failure::InvalidCatalog)?);
    let object_type = u8::try_from((bits >> 11) & 0x1f).map_err(|_| Failure::InvalidCatalog)?;
    let sampling_frequency_index =
        u8::try_from((bits >> 7) & 0x0f).map_err(|_| Failure::InvalidCatalog)?;
    let channel_configuration =
        u8::try_from((bits >> 3) & 0x0f).map_err(|_| Failure::InvalidCatalog)?;
    if object_type != 2 || sampling_frequency_index >= 13 || channel_configuration == 0 {
        return Err(Failure::UnsupportedCodec);
    }
    Ok(AacConfig {
        profile: object_type.saturating_sub(1),
        sampling_frequency_index,
        channel_configuration,
    })
}

fn adts(payload: &[u8], config: AacConfig) -> Result<Vec<u8>, Failure> {
    let frame_length = payload.len().checked_add(7).ok_or(Failure::TooLarge)?;
    if frame_length > 0x1fff {
        return Err(Failure::TooLarge);
    }
    let length = u16::try_from(frame_length).map_err(|_| Failure::TooLarge)?;
    let channels = config.channel_configuration;
    let header = [
        0xff,
        0xf1,
        (config.profile << 6) | (config.sampling_frequency_index << 2) | ((channels >> 2) & 0x01),
        ((channels & 0x03) << 6) | u8::try_from((length >> 11) & 0x03).unwrap_or(0),
        u8::try_from((length >> 3) & 0xff).unwrap_or(0),
        u8::try_from((length & 0x07) << 5).unwrap_or(0) | 0x1f,
        0xfc,
    ];
    let mut frame = Vec::with_capacity(frame_length);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(payload);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::plane::live::media::{
        Frame, FrameHeader, GroupHeader, CATALOG_VERSION, DEFAULT_MAX_GROUP_DURATION_MS,
        DEFAULT_MAX_LATENCY_MS,
    };

    fn catalog() -> Catalog {
        Catalog {
            version: CATALOG_VERSION,
            jitter_hint_ms: 50,
            tracks: vec![CatalogTrack {
                track: "video-main".into(),
                kind: TrackKind::Video,
                codec: "avc1.42c01e".into(),
                timescale: 90_000,
                decoder_config_hex: "0142c01effe100046742c01e01000268ce".into(),
                max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
                target_latency_ms: DEFAULT_MAX_LATENCY_MS,
                bitrate_bps: 2_000_000,
                width: Some(1280),
                height: Some(720),
                frame_rate_milli: Some(30_000),
                sample_rate: None,
                channels: None,
                render_group: Some("main".into()),
                cmaf_rendition: Some("main".into()),
                hls_v3_rendition: Some("main".into()),
            }],
        }
    }

    fn group(sequence: u64) -> ReceivedGroup {
        let payload = vec![0, 0, 0, 2, 0x65, 0x88];
        ReceivedGroup {
            header: GroupHeader {
                subscription_id: 2,
                track: "video-main".into(),
                track_kind: TrackKind::Video,
                group_sequence: sequence,
                published_at_micros: 1_000_000,
                timescale: 90_000,
                max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            },
            frames: vec![Frame {
                header: FrameHeader {
                    timestamp: i64::try_from(sequence).unwrap() * 90_000,
                    duration: Some(90_000),
                    timescale: 90_000,
                    kind: FrameKind::Key,
                    payload_len: u32::try_from(payload.len()).unwrap(),
                    composition_offset: 0,
                },
                payload,
            }],
        }
    }

    /// A High-profile avcC ends in an extension block; the parameter sets in
    /// front of it are the same ones a Baseline box would give.
    #[test]
    fn a_high_profile_avcc_extension_is_walked_and_not_mistaken_for_junk() {
        let sps = [0x67, 0x64, 0x00, 0x1f, 0xac];
        let pps = [0x68, 0xee, 0x3c, 0x80];
        let mut baseline = vec![1, 66, 0x00, 0x1f, 0xff, 0xe1, 0, sps.len() as u8];
        baseline.extend_from_slice(&sps);
        baseline.extend_from_slice(&[1, 0, pps.len() as u8]);
        baseline.extend_from_slice(&pps);
        let mut high = baseline.clone();
        high[1] = 100;
        // chroma_format 1, bit depths 8 and 8, one SPS extension of two bytes.
        high.extend_from_slice(&[0xfd, 0xf8, 0xf8, 1, 0, 2, 0x6d, 0x00]);
        let plain = avc_parameter_sets(&baseline).unwrap();
        assert_eq!(avc_parameter_sets(&high).unwrap(), plain);
        // A High box cut short inside its extension is still refused.
        high.truncate(high.len() - 1);
        assert_eq!(
            avc_parameter_sets(&high).unwrap_err(),
            Failure::InvalidCatalog
        );
    }

    #[test]
    fn hls_v3_segment_is_real_mpeg_ts_and_discontinuities_are_explicit() {
        let mut packager = HlsCatalogPackager::new(&catalog()).unwrap();
        assert_eq!(packager.descriptions()[0].rendition, "main");
        let first = packager.push_group(&group(1)).unwrap().unwrap();
        assert_eq!(first.duration_ms, 1_000);
        assert_eq!(first.bytes.len() % TS_PACKET_BYTES, 0);
        assert_eq!(first.bytes.first(), Some(&0x47));
        assert!(!first.discontinuity);
        let skipped = packager.push_group(&group(3)).unwrap().unwrap();
        assert!(skipped.discontinuity);
    }

    #[test]
    fn avcc_and_aac_are_converted_to_transport_elementary_streams() {
        let parameter_sets = avc_parameter_sets(
            &data_encoding::HEXLOWER
                .decode(b"0142c01effe100046742c01e01000268ce")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(&parameter_sets[..4], &[0, 0, 0, 1]);
        let video = annex_b(&[0, 0, 0, 2, 0x65, 0x88], &parameter_sets, FrameKind::Key).unwrap();
        assert!(
            video
                .windows(4)
                .filter(|bytes| *bytes == [0, 0, 0, 1])
                .count()
                >= 3
        );
        let audio = adts(&[1, 2, 3], aac_config(&[0x11, 0x90]).unwrap()).unwrap();
        assert_eq!(&audio[..2], &[0xff, 0xf1]);
        assert_eq!(audio.len(), 10);
    }
}
