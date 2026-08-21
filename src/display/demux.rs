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
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container => write!(f, "not a readable initialization segment"),
            Self::UnsupportedCodec => write!(f, "unsupported codec"),
            Self::Incomplete => write!(f, "incomplete track description"),
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::display::CmafTrackPackager;
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
