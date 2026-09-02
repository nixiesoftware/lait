//! Native live media over both contractors of the connection seam.

use std::sync::Arc;
use std::time::Duration;

use comms::mem::MemNet;
use comms::policy::Network;
use comms::{Alpn, Connection, DefaultTransport, Protocols, Transport};
use mechanics::actor::device_from_seed;
use runtime::plane::live::media::{
    self, Catalog, CatalogTrack, Control, Fetch, Frame, FrameHeader, FrameKind, GroupHeader,
    OutgoingGroup, Setup, TrackInfo, TrackKind,
};
use runtime::plane::stream_kind;

const SESSION_ALPN: Alpn = runtime::plane::LIVE_ALPN;

struct Pair {
    dialer: Box<dyn Connection>,
    accepter: Box<dyn Connection>,
    _keep: Vec<Arc<dyn Transport>>,
}

async fn mem_pair() -> Pair {
    let net = MemNet::new();
    let a: Arc<dyn Transport> = Arc::new(net.peer(device_from_seed(&[71u8; 32])));
    let b: Arc<dyn Transport> = Arc::new(net.peer(device_from_seed(&[72u8; 32])));
    let accepting = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { b.accept_connection().await })
    };
    let dialer = a
        .connect_session(b.my_id(), SESSION_ALPN)
        .await
        .expect("connect");
    let incoming = accepting.await.expect("accept task").expect("incoming");
    Pair {
        dialer,
        accepter: incoming.connection,
        _keep: vec![a, b],
    }
}

async fn iroh_pair() -> Pair {
    let protocols = Protocols {
        framed: &[],
        session: &[SESSION_ALPN],
    };
    let a = DefaultTransport::new(&[73u8; 32], &Network::Isolated, protocols)
        .await
        .expect("build A");
    let b = DefaultTransport::new(&[74u8; 32], &Network::Isolated, protocols)
        .await
        .expect("build B");
    let a_addrs = a
        .advertised_routes(Duration::from_secs(10))
        .await
        .expect("A has a route");
    let b_addrs = b
        .advertised_routes(Duration::from_secs(10))
        .await
        .expect("B has a route");
    a.learn(b.my_id(), &b_addrs);
    b.learn(a.my_id(), &a_addrs);
    let a: Arc<dyn Transport> = Arc::new(a);
    let b: Arc<dyn Transport> = Arc::new(b);
    let accepting = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { b.accept_connection().await })
    };
    let dialer = tokio::time::timeout(
        Duration::from_secs(15),
        a.connect_session(b.my_id(), SESSION_ALPN),
    )
    .await
    .expect("dial in time")
    .expect("connect");
    let incoming = tokio::time::timeout(Duration::from_secs(10), accepting)
        .await
        .expect("accept in time")
        .expect("accept task")
        .expect("incoming");
    Pair {
        dialer,
        accepter: incoming.connection,
        _keep: vec![a, b],
    }
}

async fn on_both(name: &str, property: impl AsyncFn(Pair)) {
    property(mem_pair().await).await;
    eprintln!("{name}: mem ok");
    property(iroh_pair().await).await;
    eprintln!("{name}: iroh ok");
}

fn header() -> GroupHeader {
    GroupHeader {
        subscription_id: 8,
        track: "screen/main".into(),
        track_kind: TrackKind::Video,
        group_sequence: 42,
        published_at_micros: 7_000_000,
        timescale: 90_000,
        max_group_duration_ms: media::DEFAULT_MAX_GROUP_DURATION_MS,
    }
}

fn frame(timestamp: i64, kind: FrameKind, payload: &[u8]) -> FrameHeader {
    FrameHeader {
        timestamp,
        duration: Some(3_000),
        timescale: 90_000,
        kind,
        payload_len: u32::try_from(payload.len()).expect("small fixture"),
        composition_offset: 0,
    }
}

fn catalog() -> Catalog {
    Catalog {
        version: media::CATALOG_VERSION,
        jitter_hint_ms: 250,
        tracks: vec![CatalogTrack {
            track: "screen/main".into(),
            kind: TrackKind::Video,
            codec: "avc1.640028".into(),
            timescale: 90_000,
            decoder_config_hex: "01640028".into(),
            max_group_duration_ms: media::DEFAULT_MAX_GROUP_DURATION_MS,
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
        }],
    }
}

fn track_info() -> TrackInfo {
    TrackInfo {
        track: "screen/main".into(),
        kind: TrackKind::Video,
        codec: "avc1.640028".into(),
        timescale: 90_000,
        decoder_config: vec![1, 100, 0, 40],
        max_group_duration_ms: media::DEFAULT_MAX_GROUP_DURATION_MS,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_is_one_ordered_uni_stream_on_mem_and_real_quic() {
    on_both("media Group", async |pair: Pair| {
        let key = b"key-access-unit";
        let delta = b"delta-access-unit";
        let sending = async {
            let mut group = OutgoingGroup::begin(pair.dialer.as_ref(), header(), 100)
                .await
                .expect("begin Group");
            group
                .write_frame(frame(90_000, FrameKind::Key, key), key)
                .await
                .expect("keyframe");
            group
                .write_frame(frame(93_000, FrameKind::Delta, delta), delta)
                .await
                .expect("delta frame");
            group.finish().expect("finish Group");
        };
        let receiving = async {
            let mut recv = pair
                .accepter
                .accept_uni()
                .await
                .expect("accept")
                .expect("Group flow");
            assert_eq!(
                recv.read_exact(1).await.expect("lane"),
                [stream_kind::MEDIA_GROUP]
            );
            media::read_group_body(recv.as_mut())
                .await
                .expect("valid Group")
        };
        let ((), received) = tokio::join!(sending, receiving);
        assert_eq!(received.header, header());
        assert_eq!(received.frames.len(), 2);
        assert_eq!(received.frames[0].header.kind, FrameKind::Key);
        assert_eq!(received.frames[0].payload, key);
        assert_eq!(received.frames[1].header.kind, FrameKind::Delta);
        assert_eq!(received.frames[1].payload, delta);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_catalog_update_is_one_canonical_group_on_mem_and_real_quic() {
    on_both("media catalog", async |pair: Pair| {
        let expected = catalog();
        let payload = expected.encode_canonical().expect("catalog");
        let header = GroupHeader {
            subscription_id: 2,
            track: media::CATALOG_TRACK.into(),
            track_kind: TrackKind::Catalog,
            group_sequence: 3,
            published_at_micros: 7_000_000,
            timescale: media::CATALOG_TIMESCALE,
            max_group_duration_ms: media::DEFAULT_MAX_GROUP_DURATION_MS,
        };
        let sending = async {
            let mut group = OutgoingGroup::begin(pair.dialer.as_ref(), header.clone(), 3)
                .await
                .expect("begin catalog Group");
            group
                .write_frame(
                    FrameHeader {
                        timestamp: 7_000,
                        duration: None,
                        timescale: media::CATALOG_TIMESCALE,
                        kind: FrameKind::Key,
                        payload_len: u32::try_from(payload.len()).expect("bounded catalog"),
                        composition_offset: 0,
                    },
                    &payload,
                )
                .await
                .expect("catalog Frame");
            group.finish().expect("finish catalog Group");
        };
        let receiving = async {
            let mut recv = pair
                .accepter
                .accept_uni()
                .await
                .expect("accept")
                .expect("catalog flow");
            assert_eq!(
                recv.read_exact(1).await.expect("lane"),
                [stream_kind::MEDIA_GROUP]
            );
            media::read_group_body(recv.as_mut())
                .await
                .expect("valid catalog Group")
        };
        let ((), received) = tokio::join!(sending, receiving);
        assert_eq!(Catalog::from_group(&received), Ok(expected));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_returns_one_group_on_its_request_stream_on_mem_and_real_quic() {
    on_both("media fetch", async |pair: Pair| {
        let request = Fetch {
            fetch_id: 9,
            track: "screen/main".into(),
            group_sequence: 42,
            priority: 7,
        };
        let info = track_info();
        let frames = vec![
            Frame {
                header: frame(90_000, FrameKind::Key, b"fetched-key"),
                payload: b"fetched-key".to_vec(),
            },
            Frame {
                header: frame(93_000, FrameKind::Delta, b"fetched-delta"),
                payload: b"fetched-delta".to_vec(),
            },
        ];
        let requesting = media::fetch_group(pair.dialer.as_ref(), &request, &info);
        let serving = async {
            let (mut answer, mut recv) = pair
                .accepter
                .accept_bi()
                .await
                .expect("accept")
                .expect("Fetch flow");
            assert_eq!(
                recv.read_exact(1).await.expect("lane"),
                [stream_kind::MEDIA_CONTROL]
            );
            assert_eq!(
                media::read_control_body(recv.as_mut())
                    .await
                    .expect("Fetch request"),
                Control::Fetch(request.clone())
            );
            assert!(recv.read_chunk(1).await.expect("request FIN").is_none());
            media::write_fetch_response(answer.as_mut(), &info, &frames)
                .await
                .expect("Fetch response");
        };
        let (fetched, ()) = tokio::join!(requesting, serving);
        let fetched = fetched.expect("fetched Group");
        assert_eq!(fetched.request, request);
        assert_eq!(fetched.track_info, info);
        assert_eq!(fetched.frames, frames);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_uses_its_own_bounded_lane_while_groups_use_uni_streams() {
    on_both("media control", async |pair: Pair| {
        let expected = Control::Setup(Setup {
            protocol_version: media::PROTOCOL_VERSION,
            max_group_duration_ms: media::DEFAULT_MAX_GROUP_DURATION_MS,
            max_latency_ms: 3_000,
        });
        let sending = media::send_control(pair.dialer.as_ref(), &expected);
        let receiving = async {
            let (mut answer, mut recv) = pair
                .accepter
                .accept_bi()
                .await
                .expect("accept")
                .expect("control flow");
            assert_eq!(
                recv.read_exact(1).await.expect("lane"),
                [stream_kind::MEDIA_CONTROL]
            );
            let control = media::read_control_body(recv.as_mut())
                .await
                .expect("control");
            answer.finish().expect("close response half");
            control
        };
        let (sent, received) = tokio::join!(sending, receiving);
        sent.expect("sent");
        assert_eq!(received, expected);
    })
    .await;
}
