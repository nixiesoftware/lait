//! Reading an ISO-BMFF track's shape back out of its container.
//!
//! `cmaf.rs` writes an initialization segment from a [`CatalogTrack`]. This
//! reads one back. That direction is what a stored source needs: the live plane
//! is handed a `Catalog` by whatever is encoding, and a file on disk has no
//! catalog — only a `moov` that already describes the same facts in the
//! container's own vocabulary.
//!
//! `mp4-atom` is already a dependency and already decodes every box below; it is
//! used bidirectionally in `cmaf.rs`'s own tests. Nothing new is pulled in.

use std::error::Error;
use std::fmt;

use mp4_atom::{Atom, Codec, Decode, Moov};
use runtime::plane::live::media::{
    Catalog, CatalogTrack, Frame, FrameHeader, FrameKind, GroupHeader, ReceivedGroup, TrackKind,
    CATALOG_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The bytes are not an initialization segment this can read.
    Container,
    /// The container describes a codec the display plane cannot package.
    UnsupportedCodec,
    /// A field the catalog requires is absent or out of range.
    Incomplete,
    /// The track carries composition offsets — B-frames.
    ///
    /// Both muxers write presentation time as decode time (`TrunEntry.cts` is
    /// `None`; `pts_dts` carries no DTS), so a file with a B-pyramid would be
    /// packaged with its frames in decode order and presented that way. Refused
    /// by name rather than played wrong: this plane can *read* the offsets and
    /// cannot *write* them, and the gap between those two is the whole reason
    /// this variant exists.
    CompositionOffsets,
    /// The sample table disagrees with itself.
    Malformed,
    /// This container cannot produce a catalog the plane will accept.
    ///
    /// Usually the baseline: `Catalog::validate` requires an H.264 track beside
    /// any video and an AAC track beside any audio, because that is what every
    /// receiver decodes. The rule is asked rather than restated here — a second
    /// copy of it would be a second thing to keep true — so this covers every
    /// way a catalog can fail to be valid, not only that one.
    ///
    /// It is raised where a person is standing. Deriving the catalog later, at a
    /// render or a ticket, surfaces the same refusal at a screen at three in the
    /// morning.
    Unpackageable,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container => write!(f, "not a readable initialization segment"),
            Self::UnsupportedCodec => write!(f, "unsupported codec"),
            Self::Incomplete => write!(f, "incomplete track description"),
            Self::CompositionOffsets => {
                write!(f, "composition offsets cannot be packaged by this plane")
            }
            Self::Malformed => write!(f, "the sample table disagrees with itself"),
            Self::Unpackageable => write!(f, "no valid catalog can be built from this file"),
        }
    }
}

impl Error for Failure {}

/// What a container says about one track, and nothing it does not say.
///
/// A [`CatalogTrack`](runtime::plane::live::media::CatalogTrack) needs more than
/// this — bitrate, group duration, latency budget, rendition names. Those are
/// this coordinator's policy rather than facts the file carries, so they are not
/// here to be guessed at: a shape read from a container and a shape invented by
/// a default should not be the same type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackShape {
    pub kind: TrackKind,
    /// The WebCodecs codec string, as the catalog spells it.
    pub codec: String,
    pub timescale: u32,
    /// Exactly the decoder-description bytes: `avcC` body, or AudioSpecificConfig.
    pub decoder_config: Vec<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Every track an initialization segment describes, in container order.
///
/// Tracks whose codec this plane cannot package are skipped rather than
/// refused: a file with an AV1 track beside an H.264 one is still servable, and
/// the catalog's own baseline rule is what decides whether what remains is
/// enough.
pub fn track_shapes(init: &[u8]) -> Result<Vec<TrackShape>, Failure> {
    let mut cursor = init;
    let moov = loop {
        let Ok(header) = mp4_atom::Header::decode(&mut cursor) else {
            return Err(Failure::Container);
        };
        let size = header.size.ok_or(Failure::Container)?;
        if header.kind == Moov::KIND {
            let mut body = cursor.get(..size).ok_or(Failure::Container)?;
            break Moov::decode_body(&mut body).map_err(|_| Failure::Container)?;
        }
        cursor = cursor.get(size..).ok_or(Failure::Container)?;
    };

    let mut shapes = Vec::new();
    for trak in &moov.trak {
        let timescale = trak.mdia.mdhd.timescale;
        if timescale == 0 {
            return Err(Failure::Incomplete);
        }
        for codec in &trak.mdia.minf.stbl.stsd.codecs {
            match codec {
                Codec::Avc1(avc1) => {
                    let profile = avc1.avcc.avc_profile_indication;
                    let compat = avc1.avcc.profile_compatibility;
                    let level = avc1.avcc.avc_level_indication;
                    let mut decoder_config = Vec::new();
                    avc1.avcc
                        .encode_body(&mut decoder_config)
                        .map_err(|_| Failure::Container)?;
                    shapes.push(TrackShape {
                        kind: TrackKind::Video,
                        codec: format!("avc1.{profile:02x}{compat:02x}{level:02x}"),
                        timescale,
                        decoder_config,
                        width: Some(u32::from(avc1.visual.width)),
                        height: Some(u32::from(avc1.visual.height)),
                        sample_rate: None,
                        channels: None,
                    });
                }
                Codec::Mp4a(mp4a) => {
                    let decoder_config = mp4a
                        .esds
                        .es_desc
                        .dec_config
                        .dec_specific
                        .as_ref()
                        .map(|specific| specific.raw.clone())
                        .ok_or(Failure::Incomplete)?;
                    let channels =
                        u8::try_from(mp4a.audio.channel_count).map_err(|_| Failure::Incomplete)?;
                    shapes.push(TrackShape {
                        kind: TrackKind::Audio,
                        codec: "mp4a.40.2".into(),
                        timescale,
                        decoder_config,
                        width: None,
                        height: None,
                        sample_rate: Some(u32::from(mp4a.audio.sample_rate.integer())),
                        channels: Some(channels),
                    });
                }
                _ => {}
            }
        }
    }
    Ok(shapes)
}

/// One sample's place in the file and in time.
///
/// `decode_time` and `duration` are in the track's own timescale, which is the
/// only clock the container states. Converting to anything else is the caller's
/// choice and is not made here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub decode_time: u64,
    pub duration: u32,
    /// A sync sample: a group may begin here and nowhere else.
    pub sync: bool,
}

/// Walk one track's sample table into the samples it describes.
///
/// The five boxes that have to agree: `stts` gives each sample a duration,
/// `stsz` a size, `stsc` and `stco`/`co64` a position, and `stss` says which
/// may start a group. A file where they disagree is refused rather than read to
/// whichever length runs out first — a short read here becomes a truncated film
/// that nothing reports.
pub fn samples(trak: &mp4_atom::Trak) -> Result<Vec<Sample>, Failure> {
    let stbl = &trak.mdia.minf.stbl;

    if stbl
        .ctts
        .as_ref()
        .is_some_and(|ctts| ctts.entries.iter().any(|entry| entry.sample_offset != 0))
    {
        return Err(Failure::CompositionOffsets);
    }

    let sizes: Vec<u32> = match &stbl.stsz.samples {
        mp4_atom::StszSamples::Identical { count, size } => {
            vec![*size; usize::try_from(*count).map_err(|_| Failure::Malformed)?]
        }
        mp4_atom::StszSamples::Different { sizes } => sizes.clone(),
    };
    if sizes.is_empty() {
        return Ok(Vec::new());
    }

    // `stts` is run-length encoded: each entry gives a count and the duration
    // every sample in that run carries.
    let mut durations = Vec::with_capacity(sizes.len());
    for entry in &stbl.stts.entries {
        let count = usize::try_from(entry.sample_count).map_err(|_| Failure::Malformed)?;
        durations.extend(std::iter::repeat_n(entry.sample_delta, count));
    }
    if durations.len() < sizes.len() {
        return Err(Failure::Malformed);
    }

    let chunk_offsets: Vec<u64> = match (&stbl.stco, &stbl.co64) {
        (Some(stco), _) => stco
            .entries
            .iter()
            .map(|offset| u64::from(*offset))
            .collect(),
        (None, Some(co64)) => co64.entries.clone(),
        (None, None) => return Err(Failure::Incomplete),
    };
    if chunk_offsets.is_empty() {
        return Err(Failure::Incomplete);
    }

    let sync: std::collections::BTreeSet<u32> = stbl
        .stss
        .as_ref()
        .map(|stss| stss.entries.iter().copied().collect())
        // No `stss` means every sample is a sync sample, which is what an
        // all-intra track is. Treating its absence as "none" would produce a
        // file with no legal group start at all.
        .unwrap_or_default();
    let every_sample_syncs = stbl.stss.is_none();

    let mut out = Vec::with_capacity(sizes.len());
    let mut sample_index = 0usize;
    let mut decode_time = 0u64;

    for (run, entry) in stbl.stsc.entries.iter().enumerate() {
        let first_chunk = usize::try_from(entry.first_chunk)
            .map_err(|_| Failure::Malformed)?
            .checked_sub(1)
            .ok_or(Failure::Malformed)?;
        // A run lasts until the next run's first chunk, or to the end.
        let last_chunk = match stbl.stsc.entries.get(run.saturating_add(1)) {
            Some(next) => usize::try_from(next.first_chunk)
                .map_err(|_| Failure::Malformed)?
                .checked_sub(1)
                .ok_or(Failure::Malformed)?,
            None => chunk_offsets.len(),
        };
        let per_chunk = usize::try_from(entry.samples_per_chunk).map_err(|_| Failure::Malformed)?;

        for chunk in first_chunk..last_chunk {
            let mut offset = *chunk_offsets.get(chunk).ok_or(Failure::Malformed)?;
            for _ in 0..per_chunk {
                if sample_index >= sizes.len() {
                    break;
                }
                let size = *sizes.get(sample_index).ok_or(Failure::Malformed)?;
                let duration = *durations.get(sample_index).ok_or(Failure::Malformed)?;
                // Sample numbers in `stss` are one-based.
                let number = u32::try_from(sample_index.saturating_add(1))
                    .map_err(|_| Failure::Malformed)?;
                out.push(Sample {
                    offset,
                    size,
                    decode_time,
                    duration,
                    sync: every_sample_syncs || sync.contains(&number),
                });
                offset = offset
                    .checked_add(u64::from(size))
                    .ok_or(Failure::Malformed)?;
                decode_time = decode_time
                    .checked_add(u64::from(duration))
                    .ok_or(Failure::Malformed)?;
                sample_index = sample_index.saturating_add(1);
            }
        }
    }

    if out.len() != sizes.len() {
        return Err(Failure::Malformed);
    }
    Ok(out)
}

/// Split samples into groups that begin on a sync sample and last no longer
/// than `max_group_ms`.
///
/// Both packagers require a group's first frame to be a key frame and its span
/// to fit `max_group_duration_ms`, so this produces exactly what they accept.
/// A run of samples with no sync sample in it cannot be split, and is returned
/// as one group however long it runs — the packager refuses it, which is the
/// right place for that refusal because it owns the bound.
pub fn groups(
    samples: &[Sample],
    timescale: u32,
    max_group_ms: u32,
) -> Vec<std::ops::Range<usize>> {
    if samples.is_empty() || timescale == 0 {
        return Vec::new();
    }
    let budget = u64::from(max_group_ms)
        .saturating_mul(u64::from(timescale))
        .saturating_div(1_000)
        .max(1);

    let mut out = Vec::new();
    let mut start = 0usize;
    for (index, sample) in samples.iter().enumerate().skip(1) {
        let Some(first) = samples.get(start) else {
            break;
        };
        let span = sample.decode_time.saturating_sub(first.decode_time);
        if sample.sync && span >= budget {
            out.push(start..index);
            start = index;
        }
    }
    out.push(start..samples.len());
    out
}

/// Read one track's groups, pulling sample bytes through `read`.
///
/// `read` is a seam rather than a content handle, because the demuxer's job is
/// to say *which* bytes and not to know where they live. That keeps it testable
/// against an in-memory file and lets the same code sit over a `ContentCursor`
/// without either knowing about the other.
///
/// The timestamps this produces are gapless by construction — each sample's
/// decode time is the previous one's plus its duration — which is what
/// `CmafTrackPackager::validate_group` requires and the reason the walk
/// accumulates rather than reading a per-sample time from anywhere.
pub fn track_groups(
    trak: &mp4_atom::Trak,
    shape: &TrackShape,
    track: &str,
    max_group_ms: u32,
    mut read: impl FnMut(u64, u32) -> Result<Vec<u8>, Failure>,
) -> Result<Vec<ReceivedGroup>, Failure> {
    let samples = samples(trak)?;
    let mut out = Vec::new();
    for (sequence, range) in groups(&samples, shape.timescale, max_group_ms)
        .into_iter()
        .enumerate()
    {
        let members = samples.get(range).ok_or(Failure::Malformed)?;
        let Some(first) = members.first() else {
            continue;
        };
        let mut frames = Vec::with_capacity(members.len());
        for sample in members {
            let payload = read(sample.offset, sample.size)?;
            if payload.len() != usize::try_from(sample.size).map_err(|_| Failure::Malformed)? {
                return Err(Failure::Malformed);
            }
            frames.push(Frame {
                header: FrameHeader {
                    timestamp: i64::try_from(sample.decode_time).map_err(|_| Failure::Malformed)?,
                    duration: Some(u64::from(sample.duration)),
                    timescale: shape.timescale,
                    kind: if sample.sync {
                        FrameKind::Key
                    } else {
                        FrameKind::Delta
                    },
                    payload_len: sample.size,
                },
                payload,
            });
        }
        out.push(ReceivedGroup {
            header: GroupHeader {
                subscription_id: STORED_SUBSCRIPTION,
                track: track.to_string(),
                track_kind: shape.kind,
                group_sequence: u64::try_from(sequence).map_err(|_| Failure::Malformed)?,
                // A file has no publish time. Its presentation time is the
                // honest answer, and it is what a receiver's staleness
                // arithmetic reads.
                published_at_micros: presentation_micros(first.decode_time, shape.timescale)?,
                timescale: shape.timescale,
                max_group_duration_ms: max_group_ms,
            },
            frames,
        });
    }
    Ok(out)
}

/// The subscription a stored group is attributed to.
///
/// A live group carries the id of the subscription that asked for it. Nothing
/// asked for these, so they take the first media id rather than borrowing a
/// number that means something else.
const STORED_SUBSCRIPTION: u64 = 2;

fn presentation_micros(decode_time: u64, timescale: u32) -> Result<i64, Failure> {
    if timescale == 0 {
        return Err(Failure::Incomplete);
    }
    let micros = u128::from(decode_time)
        .saturating_mul(1_000_000)
        .checked_div(u128::from(timescale))
        .ok_or(Failure::Incomplete)?;
    i64::try_from(micros).map_err(|_| Failure::Malformed)
}

/// The largest `moov` this will read into memory.
///
/// A `moov` is a table of contents; eight megabytes of one is a film with
/// millions of samples. Bounded because this runs against bytes somebody
/// uploaded, and a declared size is not a promise.
const MAX_MOOV_BYTES: u64 = 8 * 1024 * 1024;

/// One top-level box header: a 32-bit size and a four-character kind, with a
/// 64-bit size following when the 32-bit one is 1.
const BOX_HEADER_BYTES: u32 = 8;
const LARGE_BOX_HEADER_BYTES: u32 = 16;

/// Find and read the `moov`, without ever materialising the `mdat`.
///
/// A file written for streaming puts `moov` first. A file written by a camera or
/// an editor usually puts it last, after an `mdat` that is the whole film — so
/// reading a container from the front and stopping at the first big box finds
/// nothing on the common case. Walking the top-level boxes by their declared
/// size reads sixteen bytes per box and skips every payload, which is the
/// difference between reading a table of contents and reading a gigabyte.
pub fn find_moov(
    total: u64,
    mut read: impl FnMut(u64, u32) -> Result<Vec<u8>, Failure>,
) -> Result<Vec<u8>, Failure> {
    let mut at = 0u64;
    while at < total {
        let header = read(at, LARGE_BOX_HEADER_BYTES)?;
        let (size, kind, header_len) = parse_box_header(&header, total, at)?;
        if &kind == b"moov" {
            let body = size
                .checked_sub(u64::from(header_len))
                .ok_or(Failure::Container)?;
            if size > MAX_MOOV_BYTES {
                return Err(Failure::Container);
            }
            // The decoder wants the whole box, header included.
            let whole = u32::try_from(size).map_err(|_| Failure::Container)?;
            let _ = body;
            return read(at, whole);
        }
        at = at.checked_add(size).ok_or(Failure::Container)?;
    }
    Err(Failure::Container)
}

/// `(size, kind, header_len)` for the box at `at`, or a refusal.
///
/// A size of zero means "to the end of the file" and a size of one means the
/// real size follows as 64 bits. Both are legal and both are refused if they
/// would not advance — a box that does not move the cursor is how a malformed
/// file becomes an infinite loop.
fn parse_box_header(header: &[u8], total: u64, at: u64) -> Result<(u64, [u8; 4], u32), Failure> {
    let short = header.get(..8).ok_or(Failure::Container)?;
    let (declared_bytes, kind_bytes) = short.split_at(4);
    let declared = u32::from_be_bytes(declared_bytes.try_into().map_err(|_| Failure::Container)?);
    let kind: [u8; 4] = kind_bytes.try_into().map_err(|_| Failure::Container)?;
    let (size, header_len) = match declared {
        // Zero means "to the end of the file": legal, and only for the last box.
        0 => (
            total.checked_sub(at).ok_or(Failure::Container)?,
            BOX_HEADER_BYTES,
        ),
        // One means the real size follows as sixty-four bits.
        1 => {
            let large: [u8; 8] = header
                .get(8..16)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(Failure::Container)?;
            (u64::from_be_bytes(large), LARGE_BOX_HEADER_BYTES)
        }
        other => (u64::from(other), BOX_HEADER_BYTES),
    };
    if size < u64::from(header_len) || at.checked_add(size).is_none_or(|end| end > total) {
        return Err(Failure::Container);
    }
    Ok((size, kind, header_len))
}

/// What a catalog needs that a container does not carry.
///
/// Bitrate and frame rate are derived from the file's own sample table below,
/// because the file does state them. Everything here is a choice this
/// coordinator makes: how long a group may run, what latency it advertises, and
/// what the renditions are called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPolicy {
    pub max_group_duration_ms: u32,
    pub target_latency_ms: u32,
    pub jitter_hint_ms: u32,
    /// The name a receiver's ticket resolves against.
    pub rendition: String,
}

/// The mean bitrate a track's own samples imply.
pub fn bitrate_bps(samples: &[Sample], timescale: u32) -> Option<u32> {
    if samples.is_empty() || timescale == 0 {
        return None;
    }
    let bytes: u64 = samples.iter().map(|sample| u64::from(sample.size)).sum();
    let ticks: u64 = samples
        .iter()
        .map(|sample| u64::from(sample.duration))
        .sum();
    if ticks == 0 {
        return None;
    }
    let bits = u128::from(bytes)
        .saturating_mul(8)
        .saturating_mul(u128::from(timescale));
    u32::try_from(bits.checked_div(u128::from(ticks))?).ok()
}

/// The mean frame rate a track's own samples imply, in millihertz.
pub fn frame_rate_milli(samples: &[Sample], timescale: u32) -> Option<u32> {
    if samples.is_empty() || timescale == 0 {
        return None;
    }
    let ticks: u64 = samples
        .iter()
        .map(|sample| u64::from(sample.duration))
        .sum();
    if ticks == 0 {
        return None;
    }
    let count = u64::try_from(samples.len()).ok()?;
    let rate = u128::from(count)
        .saturating_mul(u128::from(timescale))
        .saturating_mul(1_000)
        .checked_div(u128::from(ticks))?;
    u32::try_from(rate).ok().filter(|rate| *rate > 0)
}

/// Build a catalog from what the container said and what this coordinator chose.
///
/// The refusal that matters is [`Failure::Unpackageable`]. `Catalog::validate`
/// requires an H.264 track beside any video and an AAC track beside any audio,
/// so a file that cannot meet it has no valid catalog at all — and this is the
/// only moment a person is present to be told. Deriving it later, at a render or
/// a ticket, would surface the same refusal at a screen at three in the morning.
pub fn catalog(
    trak: &[(mp4_atom::Trak, TrackShape)],
    policy: &CatalogPolicy,
) -> Result<Catalog, Failure> {
    let mut tracks = Vec::new();
    for (index, (trak, shape)) in trak.iter().enumerate() {
        let samples = samples(trak)?;
        let video = shape.kind == TrackKind::Video;
        tracks.push(CatalogTrack {
            track: format!("{}-{index}", if video { "video" } else { "audio" }),
            kind: shape.kind,
            codec: shape.codec.clone(),
            timescale: shape.timescale,
            decoder_config_hex: data_encoding::HEXLOWER.encode(&shape.decoder_config),
            max_group_duration_ms: policy.max_group_duration_ms,
            target_latency_ms: policy.target_latency_ms,
            bitrate_bps: bitrate_bps(&samples, shape.timescale).ok_or(Failure::Incomplete)?,
            width: shape.width,
            height: shape.height,
            frame_rate_milli: if video {
                Some(frame_rate_milli(&samples, shape.timescale).ok_or(Failure::Incomplete)?)
            } else {
                None
            },
            sample_rate: shape.sample_rate,
            channels: shape.channels,
            render_group: Some(policy.rendition.clone()),
            cmaf_rendition: Some(policy.rendition.clone()),
            // One HLS rendition per catalog: `hls_media_playlist` refuses a
            // rendition that is not the ticket's resource, so a second one
            // would be unreachable rather than an alternative.
            hls_v3_rendition: if video {
                Some(policy.rendition.clone())
            } else {
                None
            },
        });
    }
    if tracks.is_empty() {
        return Err(Failure::Unpackageable);
    }
    let catalog = Catalog {
        version: CATALOG_VERSION,
        jitter_hint_ms: policy.jitter_hint_ms,
        tracks,
    };
    // Encoding validates, and the rule is the catalog's own — asked rather than
    // restated here, because a second copy of it would be a second thing to
    // keep true.
    catalog
        .encode_canonical()
        .map_err(|_| Failure::Unpackageable)?;
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::display::CmafTrackPackager;
    use mp4_atom::{
        Ctts, CttsEntry, Stco, Stsc, StscEntry, Stss, Stsz, StszSamples, Stts, SttsEntry, Trak,
    };
    use runtime::plane::live::media::{
        CatalogTrack, DEFAULT_MAX_GROUP_DURATION_MS, DEFAULT_MAX_LATENCY_MS,
    };

    fn video_track() -> CatalogTrack {
        CatalogTrack {
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
            render_group: Some("film".into()),
            cmaf_rendition: Some("film".into()),
            hls_v3_rendition: Some("film".into()),
        }
    }

    fn audio_track() -> CatalogTrack {
        CatalogTrack {
            track: "audio-main".into(),
            kind: TrackKind::Audio,
            codec: "mp4a.40.2".into(),
            timescale: 48_000,
            decoder_config_hex: "1190".into(),
            max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            target_latency_ms: DEFAULT_MAX_LATENCY_MS,
            bitrate_bps: 128_000,
            width: None,
            height: None,
            frame_rate_milli: None,
            sample_rate: Some(48_000),
            channels: Some(2),
            render_group: Some("film".into()),
            cmaf_rendition: Some("film-audio".into()),
            hls_v3_rendition: None,
        }
    }

    /// A box of `kind` and `payload` bytes, as a container writes one.
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let size = u32::try_from(payload.len() + 8).unwrap();
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    /// A reader over an in-memory file, counting what it actually read.
    fn file_reader(
        bytes: Vec<u8>,
        read_bytes: std::rc::Rc<std::cell::Cell<u64>>,
    ) -> impl FnMut(u64, u32) -> Result<Vec<u8>, Failure> {
        move |offset, size| {
            let start = usize::try_from(offset).map_err(|_| Failure::Container)?;
            let end = start
                .checked_add(usize::try_from(size).map_err(|_| Failure::Container)?)
                .ok_or(Failure::Container)?;
            let slice = bytes.get(start..end.min(bytes.len())).unwrap_or_default();
            read_bytes.set(read_bytes.get() + slice.len() as u64);
            Ok(slice.to_vec())
        }
    }

    /// The `moov` is found after the `mdat`, and the `mdat` is never read.
    ///
    /// This is the common case, not the exotic one: a file written for
    /// streaming puts `moov` first, and a file written by a camera or an editor
    /// usually puts it last. Reading from the front and stopping at the first
    /// large box finds nothing — and reading *through* the `mdat` to reach the
    /// table of contents would pull the whole film across the content plane to
    /// learn its codec.
    #[test]
    fn a_moov_after_a_huge_mdat_is_found_without_reading_the_mdat() {
        let moov_payload = b"pretend-moov-body".to_vec();
        let mut file = boxed(b"ftyp", b"iso6");
        // A megabyte of "film" between the header and the table of contents.
        file.extend_from_slice(&boxed(b"mdat", &vec![0x11; 1_000_000]));
        let moov_at = file.len() as u64;
        file.extend_from_slice(&boxed(b"moov", &moov_payload));
        let total = file.len() as u64;

        let counted = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let found = find_moov(total, file_reader(file, counted.clone())).unwrap();

        assert_eq!(&found[4..8], b"moov");
        assert_eq!(&found[8..], &moov_payload[..]);
        assert!(
            counted.get() < 1_000,
            "walked headers rather than the film, but read {} bytes",
            counted.get()
        );
        assert!(moov_at > 1_000_000, "the moov really was past the mdat");
    }

    #[test]
    fn a_moov_at_the_front_is_found_too() {
        let mut file = boxed(b"ftyp", b"iso6");
        file.extend_from_slice(&boxed(b"moov", b"body"));
        file.extend_from_slice(&boxed(b"mdat", &vec![0x22; 4_096]));
        let total = file.len() as u64;
        let counted = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let found = find_moov(total, file_reader(file, counted)).unwrap();
        assert_eq!(&found[4..8], b"moov");
    }

    /// A container with no `moov` describes no track, and says so.
    #[test]
    fn a_container_without_a_moov_is_refused() {
        let mut file = boxed(b"ftyp", b"iso6");
        file.extend_from_slice(&boxed(b"mdat", &vec![0x33; 64]));
        let total = file.len() as u64;
        let counted = std::rc::Rc::new(std::cell::Cell::new(0u64));
        assert_eq!(
            find_moov(total, file_reader(file, counted)).unwrap_err(),
            Failure::Container
        );
    }

    /// A box that does not advance the cursor is refused rather than looped on.
    ///
    /// A declared size is not a promise — these bytes came from an upload. A
    /// zero-length box would spin `find_moov` forever, and a size past the end
    /// would have it read off the file.
    #[test]
    fn a_box_that_cannot_advance_is_refused_rather_than_looped_on() {
        // A size of 4: smaller than its own header.
        let mut stuck = Vec::new();
        stuck.extend_from_slice(&4u32.to_be_bytes());
        stuck.extend_from_slice(b"junk");
        stuck.extend_from_slice(&[0; 8]);
        let total = stuck.len() as u64;
        let counted = std::rc::Rc::new(std::cell::Cell::new(0u64));
        assert_eq!(
            find_moov(total, file_reader(stuck, counted)).unwrap_err(),
            Failure::Container
        );

        // A size past the end of the file.
        let mut over = Vec::new();
        over.extend_from_slice(&9_999u32.to_be_bytes());
        over.extend_from_slice(b"mdat");
        over.extend_from_slice(&[0; 8]);
        let total = over.len() as u64;
        let counted = std::rc::Rc::new(std::cell::Cell::new(0u64));
        assert_eq!(
            find_moov(total, file_reader(over, counted)).unwrap_err(),
            Failure::Container
        );
    }

    fn ingest_policy() -> CatalogPolicy {
        CatalogPolicy {
            max_group_duration_ms: DEFAULT_MAX_GROUP_DURATION_MS,
            target_latency_ms: DEFAULT_MAX_LATENCY_MS,
            jitter_hint_ms: 50,
            rendition: "film".into(),
        }
    }

    fn video_shape() -> TrackShape {
        TrackShape {
            kind: TrackKind::Video,
            codec: "avc1.42c01e".into(),
            timescale: 90_000,
            decoder_config: data_encoding::HEXLOWER
                .decode(b"0142c01effe100046742c01e01000268ce")
                .unwrap(),
            width: Some(1280),
            height: Some(720),
            sample_rate: None,
            channels: None,
        }
    }

    /// A catalog is built from the file's own numbers plus this coordinator's
    /// policy, and the two stay separable.
    #[test]
    fn a_catalog_takes_its_rates_from_the_file_and_its_names_from_policy() {
        // Twelve samples, 6 bytes each, one second apart at 90 kHz.
        let trak = sampled_trak(vec![6; 12], Some(vec![1]), None);
        let built = catalog(&[(trak, video_shape())], &ingest_policy()).unwrap();

        assert_eq!(built.tracks.len(), 1);
        let track = &built.tracks[0];
        // 6 bytes per second is 48 bits per second.
        assert_eq!(track.bitrate_bps, 48, "derived from the sample table");
        assert_eq!(
            track.frame_rate_milli,
            Some(1_000),
            "one sample per second is 1.000 Hz"
        );
        // Policy, not the file.
        assert_eq!(track.cmaf_rendition.as_deref(), Some("film"));
        assert_eq!(track.hls_v3_rendition.as_deref(), Some("film"));
        assert_eq!(track.max_group_duration_ms, DEFAULT_MAX_GROUP_DURATION_MS);
        assert_eq!(built.jitter_hint_ms, 50);
        // The container, unchanged.
        assert_eq!(track.codec, "avc1.42c01e");
        assert_eq!(track.timescale, 90_000);
        assert_eq!(track.width, Some(1280));
    }

    /// The catalog it produces is one the packagers accept.
    #[test]
    fn a_built_catalog_packages() {
        let trak = sampled_trak(vec![6; 6], Some(vec![1, 4]), None);
        let shape = video_shape();
        let built = catalog(&[(trak.clone(), shape.clone())], &ingest_policy()).unwrap();

        let unit = [0u8, 0, 0, 2, 0x65, 0x88];
        let groups = track_groups(&trak, &shape, &built.tracks[0].track, 2_000, |_, _| {
            Ok(unit.to_vec())
        })
        .unwrap();

        let hub = crate::display::LiveMediaHub::default();
        hub.install_whole("space/orbit", "film", &built, groups)
            .expect("a catalog derived at ingest packages at serve");
        assert!(hub
            .hls_media_playlist("space/orbit", "film", "film", "..")
            .unwrap()
            .contains("#EXT-X-ENDLIST"));
    }

    /// A file that cannot meet the baseline is refused at ingest.
    ///
    /// The person who uploaded it is standing there. The same refusal derived
    /// at a render would arrive at a screen instead.
    #[test]
    fn a_file_that_cannot_meet_the_baseline_is_refused_where_a_person_is() {
        let trak = sampled_trak(vec![6; 3], Some(vec![1]), None);
        let mut av1 = video_shape();
        av1.codec = "av01.0.04M.08".into();
        assert_eq!(
            catalog(&[(trak, av1)], &ingest_policy()).unwrap_err(),
            Failure::Unpackageable,
            "any video needs an H.264 track beside it"
        );

        assert_eq!(
            catalog(&[], &ingest_policy()).unwrap_err(),
            Failure::Unpackageable,
            "a container with no packageable track has no catalog"
        );
    }

    /// A track whose sample table this test states outright, so the walk is
    /// checked against an answer nothing derived from the walk.
    ///
    /// Two chunks of three samples each, at file offsets 1000 and 5000; every
    /// sample 90000 ticks long (one second at 90 kHz); samples 1 and 4 sync.
    const SAMPLES_PER_CHUNK: usize = 3;

    fn sampled_trak(sizes: Vec<u32>, stss: Option<Vec<u32>>, ctts: Option<Ctts>) -> Trak {
        let sample_count = sizes.len();
        let mut trak = Trak::default();
        trak.mdia.mdhd.timescale = 90_000;
        trak.mdia.minf.stbl.stts = Stts {
            entries: vec![SttsEntry {
                sample_count: u32::try_from(sizes.len()).unwrap(),
                sample_delta: 90_000,
            }],
        };
        trak.mdia.minf.stbl.stsz = Stsz {
            samples: StszSamples::Different { sizes },
        };
        trak.mdia.minf.stbl.stsc = Stsc {
            entries: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: u32::try_from(SAMPLES_PER_CHUNK).unwrap(),
                sample_description_index: 1,
            }],
        };
        // One offset per chunk, derived so the boxes cannot disagree by
        // accident — a fixture that contradicts itself tests the refusal
        // instead of the walk.
        let chunks = sample_count.div_ceil(SAMPLES_PER_CHUNK);
        trak.mdia.minf.stbl.stco = Some(Stco {
            entries: (0..chunks)
                .map(|chunk| 1_000 + u32::try_from(chunk).unwrap() * 4_000)
                .collect(),
        });
        trak.mdia.minf.stbl.stss = stss.map(|entries| Stss { entries });
        trak.mdia.minf.stbl.ctts = ctts;
        trak
    }

    #[test]
    fn a_sample_table_walks_to_offsets_times_and_sync_flags() {
        let trak = sampled_trak(vec![10, 20, 30, 40, 50, 60], Some(vec![1, 4]), None);
        let samples = samples(&trak).unwrap();

        assert_eq!(samples.len(), 6);
        // Chunk one starts at 1000; each sample follows the one before it.
        assert_eq!(samples[0].offset, 1_000);
        assert_eq!(samples[1].offset, 1_010);
        assert_eq!(samples[2].offset, 1_030);
        // Chunk two starts where `stco` says, not where chunk one ended.
        assert_eq!(samples[3].offset, 5_000);
        assert_eq!(samples[4].offset, 5_040);
        assert_eq!(samples[5].offset, 5_090);

        assert_eq!(samples[0].decode_time, 0);
        assert_eq!(samples[3].decode_time, 3 * 90_000);
        assert!(samples.iter().all(|sample| sample.duration == 90_000));

        let syncs: Vec<bool> = samples.iter().map(|sample| sample.sync).collect();
        assert_eq!(syncs, vec![true, false, false, true, false, false]);
    }

    /// No `stss` means every sample is a sync sample — an all-intra track.
    ///
    /// Reading its absence as "none are sync" would leave a track with no legal
    /// group start at all, and the packagers would refuse every group of a file
    /// that is in fact the easiest kind to segment.
    #[test]
    fn a_track_without_a_sync_table_is_all_sync_not_none() {
        let trak = sampled_trak(vec![10, 20, 30], None, None);
        let samples = samples(&trak).unwrap();
        assert!(samples.iter().all(|sample| sample.sync));
    }

    /// B-frames are refused by name rather than presented out of order.
    #[test]
    fn composition_offsets_are_refused_because_neither_muxer_can_write_them() {
        let trak = sampled_trak(
            vec![10, 20, 30],
            Some(vec![1]),
            Some(Ctts {
                entries: vec![CttsEntry {
                    sample_count: 3,
                    sample_offset: 3_000,
                }],
            }),
        );
        assert_eq!(samples(&trak).unwrap_err(), Failure::CompositionOffsets);
    }

    /// A `ctts` whose offsets are all zero is a file that carries the box and
    /// no B-frames, which is common and perfectly packageable.
    #[test]
    fn a_zero_composition_table_is_not_a_b_frame() {
        let trak = sampled_trak(
            vec![10, 20, 30],
            Some(vec![1]),
            Some(Ctts {
                entries: vec![CttsEntry {
                    sample_count: 3,
                    sample_offset: 0,
                }],
            }),
        );
        assert_eq!(samples(&trak).unwrap().len(), 3);
    }

    /// A table that disagrees with itself is refused, not read to whichever
    /// box runs out first — that would be a truncated film nothing reports.
    #[test]
    fn a_table_that_disagrees_with_itself_is_refused() {
        let mut trak = sampled_trak(vec![10, 20, 30, 40, 50, 60], Some(vec![1]), None);
        // Six sizes, three durations.
        trak.mdia.minf.stbl.stts = Stts {
            entries: vec![SttsEntry {
                sample_count: 3,
                sample_delta: 90_000,
            }],
        };
        assert_eq!(samples(&trak).unwrap_err(), Failure::Malformed);

        let mut headless = sampled_trak(vec![10], Some(vec![1]), None);
        headless.mdia.minf.stbl.stco = None;
        assert_eq!(samples(&headless).unwrap_err(), Failure::Incomplete);
    }

    /// Groups begin on a sync sample and last no longer than their budget.
    #[test]
    fn groups_start_on_sync_samples_and_respect_the_duration_budget() {
        let trak = sampled_trak(
            vec![10; 12],
            // Every third sample is a key frame: 1, 4, 7, 10.
            Some(vec![1, 4, 7, 10]),
            None,
        );
        let samples = samples(&trak).unwrap();

        // Each sample is one second, so a two-second budget closes a group at
        // the first key frame at or past two seconds — which is sample 4.
        let grouped = groups(&samples, 90_000, 2_000);
        assert_eq!(grouped, vec![0..3, 3..6, 6..9, 9..12]);

        // A budget longer than any gap between key frames yields one group.
        assert_eq!(groups(&samples, 90_000, 60_000), vec![0..12]);

        // Every group starts on a sync sample, which is what the packagers
        // require and the only reason this function exists.
        for range in groups(&samples, 90_000, 2_000) {
            assert!(samples[range.start].sync);
        }
    }

    #[test]
    fn an_empty_track_has_no_samples_and_no_groups() {
        let trak = sampled_trak(Vec::new(), Some(Vec::new()), None);
        assert!(samples(&trak).unwrap().is_empty());
        assert!(groups(&[], 90_000, 2_000).is_empty());
    }

    /// A track reads end to end into groups the packagers accept.
    ///
    /// The reader here is a plain in-memory file, which is the point of taking
    /// one: the demuxer says which bytes and knows nothing about where they
    /// live, so this runs with no Station, no content plane and no network.
    #[test]
    fn a_track_reads_into_groups_the_packagers_accept() {
        let trak = sampled_trak(vec![6; 12], Some(vec![1, 4, 7, 10]), None);
        let shape = TrackShape {
            kind: TrackKind::Video,
            codec: "avc1.42c01e".into(),
            timescale: 90_000,
            decoder_config: Vec::new(),
            width: Some(1280),
            height: Some(720),
            sample_rate: None,
            channels: None,
        };
        // One AVCC access unit: a four-byte length and a two-byte IDR NAL.
        let unit = [0u8, 0, 0, 2, 0x65, 0x88];
        let groups = track_groups(&trak, &shape, "video-main", 2_000, |_offset, size| {
            assert_eq!(size, 6);
            Ok(unit.to_vec())
        })
        .unwrap();

        assert_eq!(groups.len(), 4, "one group per key frame at this budget");
        assert_eq!(groups[0].header.group_sequence, 0);
        assert_eq!(groups[3].header.group_sequence, 3);
        assert_eq!(groups[0].header.track, "video-main");
        assert_eq!(groups[0].header.timescale, 90_000);
        assert_eq!(groups[0].header.published_at_micros, 0);
        // Group two starts at sample four, three seconds in.
        assert_eq!(groups[1].header.published_at_micros, 3_000_000);

        for group in &groups {
            assert_eq!(group.frames.len(), 3);
            assert_eq!(
                group.frames[0].header.kind,
                FrameKind::Key,
                "every group starts on a key frame, which both packagers require"
            );
            // Gapless within the group: each timestamp is the previous plus its
            // duration, which is what `validate_group` checks.
            for pair in group.frames.windows(2) {
                let previous = &pair[0].header;
                let next = &pair[1].header;
                assert_eq!(
                    next.timestamp,
                    previous.timestamp + i64::try_from(previous.duration.unwrap()).unwrap()
                );
            }
        }
    }

    /// A reader that returns the wrong number of bytes is a truncated file, and
    /// it is caught here rather than becoming a group the packager accepts.
    #[test]
    fn a_short_read_is_refused_rather_than_packaged() {
        let trak = sampled_trak(vec![6; 3], Some(vec![1]), None);
        let shape = TrackShape {
            kind: TrackKind::Video,
            codec: "avc1.42c01e".into(),
            timescale: 90_000,
            decoder_config: Vec::new(),
            width: Some(1280),
            height: Some(720),
            sample_rate: None,
            channels: None,
        };
        let result = track_groups(&trak, &shape, "video-main", 2_000, |_offset, _size| {
            Ok(vec![0, 0, 0, 2])
        });
        assert_eq!(result.unwrap_err(), Failure::Malformed);
    }

    /// The groups this produces are the groups the real packager takes.
    ///
    /// Asserting the shape by hand would only pin what this file believes.
    /// Running them through `HlsCatalogPackager` pins what the consumer
    /// accepts, which is the property that matters.
    #[test]
    fn the_groups_a_track_produces_package_into_real_segments() {
        let trak = sampled_trak(vec![6; 6], Some(vec![1, 4]), None);
        let shape = TrackShape {
            kind: TrackKind::Video,
            codec: "avc1.42c01e".into(),
            timescale: 90_000,
            decoder_config: Vec::new(),
            width: Some(1280),
            height: Some(720),
            sample_rate: None,
            channels: None,
        };
        let unit = [0u8, 0, 0, 2, 0x65, 0x88];
        let groups = track_groups(&trak, &shape, "video-main", 2_000, |_offset, _size| {
            Ok(unit.to_vec())
        })
        .unwrap();

        let hub = crate::display::LiveMediaHub::default();
        hub.install_whole("space/orbit", "film", &demuxed_catalog(), groups)
            .expect("demuxed groups install through the real packagers");
        let playlist = hub
            .hls_media_playlist("space/orbit", "film", "film", "..")
            .unwrap();
        assert!(playlist.trim_end().ends_with("#EXT-X-ENDLIST"));
        assert!(hub.hls_segment("space/orbit", "film", "film", 0).is_ok());
    }

    /// The catalog those groups are packaged against, with the policy fields a
    /// container does not carry.
    fn demuxed_catalog() -> runtime::plane::live::media::Catalog {
        runtime::plane::live::media::Catalog {
            version: runtime::plane::live::media::CATALOG_VERSION,
            jitter_hint_ms: 50,
            tracks: vec![video_track()],
        }
    }

    /// The reader agrees with the writer, on the writer's own output.
    ///
    /// This is the property worth having: `cmaf.rs` builds an initialization
    /// segment from a catalog track, and reading it back must recover the facts
    /// the container was given. A parser tested only against files somebody
    /// else wrote can drift from the encoder in this same repo and nothing
    /// says so.
    #[test]
    fn a_video_track_survives_the_round_trip_through_its_own_init_segment() {
        let track = video_track();
        let packager = CmafTrackPackager::new(&track).unwrap();
        let shapes = track_shapes(packager.init_segment()).unwrap();

        assert_eq!(shapes.len(), 1);
        let shape = &shapes[0];
        assert_eq!(shape.kind, TrackKind::Video);
        assert_eq!(shape.codec, track.codec, "profile, compatibility and level");
        assert_eq!(shape.timescale, track.timescale);
        assert_eq!(shape.width, track.width);
        assert_eq!(shape.height, track.height);
        assert_eq!(
            data_encoding::HEXLOWER.encode(&shape.decoder_config),
            track.decoder_config_hex,
            "exactly the bytes the catalog carried"
        );
    }

    #[test]
    fn an_audio_track_survives_the_round_trip_through_its_own_init_segment() {
        let track = audio_track();
        let packager = CmafTrackPackager::new(&track).unwrap();
        let shapes = track_shapes(packager.init_segment()).unwrap();

        assert_eq!(shapes.len(), 1);
        let shape = &shapes[0];
        assert_eq!(shape.kind, TrackKind::Audio);
        assert_eq!(shape.codec, "mp4a.40.2");
        assert_eq!(shape.timescale, track.timescale);
        assert_eq!(shape.sample_rate, track.sample_rate);
        assert_eq!(shape.channels, track.channels);
        assert_eq!(
            data_encoding::HEXLOWER.encode(&shape.decoder_config),
            track.decoder_config_hex
        );
    }

    /// Bytes that are not a container are refused, rather than read as an empty
    /// track list — which would present as "this file has no video".
    #[test]
    fn bytes_that_are_not_a_container_are_refused_not_read_as_empty() {
        assert_eq!(track_shapes(&[]).unwrap_err(), Failure::Container);
        assert_eq!(track_shapes(b"not an mp4").unwrap_err(), Failure::Container);
        assert_eq!(
            track_shapes(&[0, 0, 0, 8, b'f', b't', b'y', b'p']).unwrap_err(),
            Failure::Container,
            "a container with no moov describes no track"
        );
    }
}
