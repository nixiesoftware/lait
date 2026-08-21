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
use runtime::plane::live::media::TrackKind;

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
