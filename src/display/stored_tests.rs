//! The stored-media chain, tested against its consumers.
//!
//! `mediabox` reads a container into the live plane's vocabulary and tests
//! itself against fixtures. What it cannot test is the property that
//! matters most — that what it reads is what the packagers and the hub
//! accept — because those are this binary's, and a crate cannot depend on
//! its consumer. These are those tests, moved here rather than weakened.

use mediabox::testkit::*;
use mediabox::{catalog, read_catalog, track_groups, track_shapes, TrackShape};
use mp4_atom::{Decode, Stco};
use runtime::plane::live::media::TrackKind;

use super::{CmafTrackPackager, LiveMediaHub};

/// A whole file, read the way a stored upload is read, and served.
///
/// Everything else here tests a piece against a fixture. This builds a real
/// container — `ftyp`, then an `mdat` holding the samples, then a `moov`
/// after it, which is where a camera or an editor puts one — reads it
/// through `read_catalog` and `groups` with nothing but a byte reader, and
/// carries the result into the real packagers.
///
/// If ingest and serve ever disagree about a shape, this is what says so.
#[test]
fn a_whole_file_reads_from_its_bytes_and_serves() {
    use mp4_atom::{Avc1, Avcc, Encode, FourCC, Ftyp, Mdhd, Moov, Mvhd, Stsd, Visual};

    let unit = [0u8, 0, 0, 2, 0x65, 0x88];
    let file = whole_file();
    let total = u64::try_from(file.len()).unwrap();
    let counted = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let bytes = file.clone();

    let media = read_catalog(
        total,
        file_reader(bytes.clone(), counted.clone()),
        &ingest_policy(),
    )
    .expect("a real container yields a catalog");
    assert_eq!(media.catalog.tracks.len(), 1);
    assert_eq!(media.catalog.tracks[0].codec, "avc1.42c01e");
    assert_eq!(media.catalog.tracks[0].width, Some(1280));

    let groups = media
        .groups(2_000, file_reader(bytes, counted))
        .expect("groups read from the same bytes");
    assert_eq!(groups.len(), 2, "two key frames at this budget");
    // The bytes really came out of the mdat, at the offsets the table gave.
    assert_eq!(groups[0].frames[0].payload, unit.to_vec());

    let hub = LiveMediaHub::default();
    hub.install_whole("space/orbit", "film", &media.catalog, groups)
        .expect("what ingest read is what serve packages");
    let playlist = hub
        .hls_media_playlist("space/orbit", "film", "film", "..")
        .unwrap();
    assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
    assert!(playlist.trim_end().ends_with("#EXT-X-ENDLIST"));
    assert!(hub.hls_segment("space/orbit", "film", "film", 0).is_ok());
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

    let hub = LiveMediaHub::default();
    hub.install_whole("space/orbit", "film", &built, groups)
        .expect("a catalog derived at ingest packages at serve");
    assert!(hub
        .hls_media_playlist("space/orbit", "film", "film", "..")
        .unwrap()
        .contains("#EXT-X-ENDLIST"));
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

    let hub = LiveMediaHub::default();
    hub.install_whole("space/orbit", "film", &demuxed_catalog(), groups)
        .expect("demuxed groups install through the real packagers");
    let playlist = hub
        .hls_media_playlist("space/orbit", "film", "film", "..")
        .unwrap();
    assert!(playlist.trim_end().ends_with("#EXT-X-ENDLIST"));
    assert!(hub.hls_segment("space/orbit", "film", "film", 0).is_ok());
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
