//! Plan 14 S0 — the frozen protocol shapes, and what they refuse.
//!
//! S0 builds no handler and no routing. What it freezes is the vocabulary S1
//! and later are built against, and the point of freezing it first is that
//! three planes growing three bespoke framings is a thing that happens when
//! nobody writes the shapes down.
//!
//! Every decode test below is really a bounds test. Remote input is hostile by
//! definition, and the property that matters is not "we reject bad frames" but
//! "we reject them *before* we allocate for them".

use runtime::planes::{
    bounds, datagram_fits, feature, stream_kind, FreightFrame, Plane, PlaneWireError,
    ProtocolCapability, SessionAccept, SessionOpen, FREIGHT_ALPN, FREIGHT_PROTOCOL_VERSION,
    LIVE_ALPN, LIVE_PROTOCOL_VERSION, MAX_PROOF_BYTES, MAX_WANTED_CHUNKS, SPACE_ID_LEN,
};

fn opening(plane: Plane) -> SessionOpen {
    SessionOpen {
        plane,
        protocol_version: plane.protocol_version(),
        features: feature::RESIDENCY_HINTS,
        space: [7u8; SPACE_ID_LEN],
        initiator_station: [1u8; 32],
        responder_station: [2u8; 32],
        session_id: [3u8; 16],
        session_epoch: [4u8; 16],
        authority_frontier: vec![9],
        requested_lanes: vec![stream_kind::CONTROL],
    }
}

#[test]
fn the_two_planes_have_distinct_alpns_and_the_alpn_is_the_version_gate() {
    assert_ne!(FREIGHT_ALPN, LIVE_ALPN);
    assert_eq!(Plane::Freight.alpn(), FREIGHT_ALPN);
    assert_eq!(Plane::Live.alpn(), LIVE_ALPN);
    // The generation lives in the ALPN string, so two peers on different
    // generations share no ALPN and never connect. Nothing in-band re-checks
    // it, which is the point — but the opening still carries the number so a
    // mismatch inside one generation is caught rather than misread.
    assert!(String::from_utf8_lossy(FREIGHT_ALPN).ends_with("/1"));
    assert!(String::from_utf8_lossy(LIVE_ALPN).ends_with("/1"));
    assert_eq!(Plane::Freight.protocol_version(), FREIGHT_PROTOCOL_VERSION);
    assert_eq!(Plane::Live.protocol_version(), LIVE_PROTOCOL_VERSION);
}

#[test]
fn an_opening_roundtrips_canonically() {
    for plane in [Plane::Freight, Plane::Live] {
        let open = opening(plane);
        let encoded = open.encode();
        assert_eq!(SessionOpen::decode_canonical(&encoded).unwrap(), open);

        let mut extended = encoded.clone();
        extended.push(0);
        assert_eq!(
            SessionOpen::decode_canonical(&extended),
            Err(PlaneWireError::NonCanonical)
        );
        assert_eq!(
            SessionOpen::decode_canonical(&encoded[..encoded.len() - 1]),
            Err(PlaneWireError::NonCanonical)
        );
    }
}

#[test]
fn an_oversized_opening_is_refused_by_length_before_it_is_decoded() {
    // The ordering that matters: a peer must not be able to make us allocate a
    // decode buffer by declaring one.
    let huge = vec![0u8; bounds::MAX_OPENING_BYTES + 1];
    assert_eq!(
        SessionOpen::decode_canonical(&huge),
        Err(PlaneWireError::TooLarge)
    );
    let huge_accept = vec![0u8; bounds::MAX_OPENING_BYTES + 1];
    assert_eq!(
        SessionAccept::decode_canonical(&huge_accept),
        Err(PlaneWireError::TooLarge)
    );
}

#[test]
fn a_generation_this_build_does_not_speak_is_named() {
    let mut open = opening(Plane::Freight);
    open.protocol_version = 99;
    assert_eq!(
        SessionOpen::decode_canonical(&open.encode()),
        Err(PlaneWireError::UnsupportedVersion(99))
    );
}

#[test]
fn a_reserved_stream_kind_is_known_and_unimplemented() {
    // Different answers for different things. Unknown means reset the stream;
    // reserved means a peer is speaking a protocol we allocated and have not
    // built, which is a version problem rather than a malformed one.
    for kind in [stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL] {
        assert!(stream_kind::is_implemented(kind));
        assert!(!stream_kind::is_reserved(kind));
    }
    for kind in [
        stream_kind::RESERVED_MEDIA_FRAME,
        stream_kind::RESERVED_MEDIA_FEEDBACK,
    ] {
        assert!(stream_kind::is_reserved(kind));
        assert!(
            !stream_kind::is_implemented(kind),
            "reserving a kind is the promise that it is not built"
        );
    }
    assert!(!stream_kind::is_implemented(0x99));
    assert!(!stream_kind::is_reserved(0x99));
}

#[test]
fn an_opening_requesting_an_unimplemented_lane_is_refused() {
    let mut open = opening(Plane::Live);
    open.requested_lanes = vec![stream_kind::RESERVED_MEDIA_FRAME];
    assert_eq!(
        SessionOpen::decode_canonical(&open.encode()),
        Err(PlaneWireError::UnknownStreamKind(
            stream_kind::RESERVED_MEDIA_FRAME
        ))
    );
}

#[test]
fn lane_and_frontier_counts_are_bounded() {
    let mut open = opening(Plane::Live);
    open.requested_lanes = vec![stream_kind::CONTROL; bounds::MAX_LANES + 1];
    assert_eq!(
        SessionOpen::decode_canonical(&open.encode()),
        Err(PlaneWireError::Bounds)
    );

    let mut open = opening(Plane::Live);
    open.authority_frontier = vec![0u8; bounds::MAX_CONTROL_FRAME_BYTES + 1];
    // Too large to even be an opening, so the length gate fires first — which
    // is the correct order.
    assert_eq!(
        SessionOpen::decode_canonical(&open.encode()),
        Err(PlaneWireError::TooLarge)
    );
}

#[test]
fn a_replay_is_recognisable_from_the_opening_alone() {
    // 0.5-RTT data can be replayed by an interceptor, so accepting an opening
    // has to be idempotent — and that is only possible if a replay can be
    // told apart from a new session without any other state.
    let first = opening(Plane::Freight);
    let replay = first.clone();
    assert!(first.is_replay_of(&replay));

    let mut reconnect = first.clone();
    reconnect.session_epoch = [5u8; 16];
    assert!(
        !first.is_replay_of(&reconnect),
        "a reconnect mints a new epoch and is a new session"
    );

    let mut other = first.clone();
    other.session_id = [6u8; 16];
    assert!(!first.is_replay_of(&other));
}

#[test]
fn absent_features_decode_to_none_rather_than_failing() {
    // How an older build's advertisement is read. A peer acts on a bit only if
    // the other side set it, so "no bits" must be a valid answer and not a
    // parse error — otherwise every additive capability would need an ALPN
    // bump, which is exactly what feature bits exist to avoid.
    let capability = ProtocolCapability {
        plane: Plane::Freight,
        protocol_version: FREIGHT_PROTOCOL_VERSION,
        features: 0,
    };
    let encoded = postcard::to_stdvec(&capability).unwrap();
    let back: ProtocolCapability = postcard::from_bytes(&encoded).unwrap();
    assert_eq!(back.features, 0);
    assert_eq!(back, capability);
    assert_ne!(feature::UNSOLICITED_PROVIDE, feature::RESIDENCY_HINTS);
}

#[test]
fn freight_requests_are_exact_and_bounded() {
    let get = FreightFrame::GetChunk {
        content_id: [1u8; 32],
        chunk_index: 3,
        offset: 0,
        max_len: 1024,
        resume_leaf: None,
    };
    assert_eq!(FreightFrame::decode_canonical(&get.encode()).unwrap(), get);

    let overlong = FreightFrame::GetChunk {
        content_id: [1u8; 32],
        chunk_index: 3,
        offset: 0,
        max_len: bounds::MAX_CHUNK_FRAME_BYTES as u32 + 1,
        resume_leaf: None,
    };
    assert_eq!(
        FreightFrame::decode_canonical(&overlong.encode()),
        Err(PlaneWireError::Bounds)
    );

    let greedy = FreightFrame::Have {
        content_id: [1u8; 32],
        wanted: vec![0; MAX_WANTED_CHUNKS + 1],
    };
    assert_eq!(
        FreightFrame::decode_canonical(&greedy.encode()),
        Err(PlaneWireError::Bounds),
        "a request naming more chunks than any content has is refused by count"
    );

    let fat_proof = FreightFrame::ChunkHeader {
        content_id: [1u8; 32],
        chunk_index: 0,
        proof: vec![0u8; MAX_PROOF_BYTES + 1],
        total_len: 10,
    };
    assert_eq!(
        FreightFrame::decode_canonical(&fat_proof.encode()),
        Err(PlaneWireError::Bounds)
    );
}

#[test]
fn a_refusal_says_nothing_about_why() {
    // A provider may refuse because of authorization, policy, load, absence, or
    // incomplete proof material. Distinguishing them would let a peer probe
    // for what a Space holds by asking and reading the error.
    let refused = FreightFrame::Refused;
    let encoded = refused.encode();
    assert_eq!(FreightFrame::decode_canonical(&encoded).unwrap(), refused);
    assert!(
        encoded.len() <= 2,
        "a refusal carries no payload to read meaning out of: {} bytes",
        encoded.len()
    );
}

#[test]
fn the_raw_flow_ceiling_is_far_below_the_framed_stream_guard() {
    // comms::MAX_FRAME is 64 MiB, sized for whole protocol messages on the
    // existing framed Stream. A raw flow is read incrementally, so inheriting
    // that would mean 64 MiB of pre-allocation per concurrent flow — which is
    // how a handful of transfers exhausts a receiver.
    assert!(
        (bounds::MAX_FLOW_READ_BYTES as u32) < comms::MAX_FRAME / 64,
        "the flow ceiling must be orders below the frame guard"
    );
    // And a chunk frame has to fit one content chunk plus its envelope and
    // proof, or the frozen geometry could not be transferred at all.
    assert!(
        bounds::MAX_CHUNK_FRAME_BYTES > replica::content::max_ciphertext_len() + MAX_PROOF_BYTES
    );
}

#[test]
fn the_datagram_ceiling_is_advisory_and_the_path_can_be_smaller() {
    // Measured: two runs of comms::transport_capabilities on one machine and a
    // direct path reported 1382 then 1162. The second is below this constant,
    // so a sender that trusted the constant alone would have been refused.
    assert!(bounds::MAX_DATAGRAM_BYTES <= 1_200);
    assert!(datagram_fits(1_200, Some(1_382)));
    assert!(
        !datagram_fits(1_200, Some(1_162)),
        "the path's current capacity wins even when it is below our own bound"
    );
    assert!(!datagram_fits(bounds::MAX_DATAGRAM_BYTES + 1, Some(9_000)));
    assert!(
        !datagram_fits(1, None),
        "no negotiated datagram support is a refusal, not an unlimited path"
    );
}

#[test]
fn a_signal_cannot_exceed_its_hard_ceiling() {
    assert_eq!(bounds::MAX_SIGNAL_BYTES, 16 * 1024);
    assert!(bounds::MAX_SIGNAL_BYTES < bounds::MAX_CONTROL_FRAME_BYTES);
}

// ---------------------------------------------------------------------------
// Golden encodings
// ---------------------------------------------------------------------------

/// Hex, so a diff shows which byte moved rather than that something did.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn every_plane_shape_has_a_frozen_encoding() {
    // Identifiers carry no version suffix, so nothing in the *source* records
    // which generation a shape belongs to. These bytes are that record. A
    // refactor that changes an encoding without changing an ALPN is the failure
    // this catches, and it is silent by construction otherwise.
    let open = SessionOpen {
        plane: Plane::Freight,
        protocol_version: FREIGHT_PROTOCOL_VERSION,
        features: feature::UNSOLICITED_PROVIDE | feature::RESIDENCY_HINTS,
        space: [7u8; SPACE_ID_LEN],
        initiator_station: [1u8; 32],
        responder_station: [2u8; 32],
        session_id: [3u8; 16],
        session_epoch: [4u8; 16],
        authority_frontier: vec![9, 8, 7],
        requested_lanes: vec![stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL],
    };
    let accept = SessionAccept {
        session_id: [3u8; 16],
        session_epoch: [4u8; 16],
        capability: ProtocolCapability {
            plane: Plane::Freight,
            protocol_version: FREIGHT_PROTOCOL_VERSION,
            features: feature::RESIDENCY_HINTS,
        },
        granted_lanes: vec![stream_kind::CONTROL],
    };

    let goldens: Vec<(&str, Vec<u8>)> = vec![
        ("SessionOpen", open.encode()),
        ("SessionAccept", accept.encode()),
        (
            "Have",
            FreightFrame::Have {
                content_id: [5u8; 32],
                wanted: vec![0, 3, 9],
            }
            .encode(),
        ),
        (
            "Available",
            FreightFrame::Available {
                content_id: [5u8; 32],
                chunks: vec![0, 9],
            }
            .encode(),
        ),
        (
            "GetChunk",
            FreightFrame::GetChunk {
                content_id: [5u8; 32],
                chunk_index: 3,
                offset: 1024,
                max_len: 262_144,
                resume_leaf: Some([6u8; 32]),
            }
            .encode(),
        ),
        (
            "ChunkHeader",
            FreightFrame::ChunkHeader {
                content_id: [5u8; 32],
                chunk_index: 3,
                proof: vec![1, 2, 3, 4],
                total_len: 262_144,
            }
            .encode(),
        ),
        ("Refused", FreightFrame::Refused.encode()),
    ];

    let rendered: Vec<String> = goldens
        .iter()
        .map(|(name, bytes)| format!("{name} {}", hex(bytes)))
        .collect();
    let frozen = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plane_goldens.txt"),
    )
    .unwrap_or_default();
    let frozen: Vec<String> = frozen
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if frozen.is_empty() {
        std::fs::write(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plane_goldens.txt"),
            rendered.join("\n") + "\n",
        )
        .expect("write goldens");
        panic!("goldens were missing and have been written; re-run to verify");
    }
    assert_eq!(rendered, frozen, "an encoded plane shape moved");

    // And every golden still decodes to what produced it.
    assert_eq!(SessionOpen::decode_canonical(&open.encode()).unwrap(), open);
    assert_eq!(
        SessionAccept::decode_canonical(&accept.encode()).unwrap(),
        accept
    );
    for (_, bytes) in goldens.iter().skip(2) {
        FreightFrame::decode_canonical(bytes).expect("a golden frame decodes");
    }
}

#[test]
fn a_frame_we_should_not_have_written_becomes_a_refusal_rather_than_a_protocol_error() {
    // A bound checked only on receive turns a local mistake into a remote
    // protocol error, and the failure is then attributed to the wrong side.
    let oversized = FreightFrame::Available {
        content_id: [1u8; 32],
        chunks: vec![0; MAX_WANTED_CHUNKS + 1],
    };
    assert_eq!(
        FreightFrame::decode_canonical(&oversized.encode_bounded()).unwrap(),
        FreightFrame::Refused
    );
    // A legal frame is untouched.
    let fine = FreightFrame::Available {
        content_id: [1u8; 32],
        chunks: vec![0, 1, 2],
    };
    assert_eq!(fine.encode_bounded(), fine.encode());
}

#[test]
fn the_openings_inner_bound_is_one_that_can_actually_fire() {
    // The outer length gate refuses anything past MAX_OPENING_BYTES, so an
    // inner check at a larger number is unreachable — and an unreachable bound
    // reads like protection while providing none.
    let mut open = opening(Plane::Live);
    open.authority_frontier = vec![0u8; bounds::MAX_OPENING_BYTES / 2 + 1];
    assert_eq!(
        SessionOpen::decode_canonical(&open.encode()),
        Err(PlaneWireError::Bounds),
        "the inner bound fires before the outer one"
    );
    let mut open = opening(Plane::Live);
    open.authority_frontier = vec![0u8; bounds::MAX_OPENING_BYTES / 2 - 64];
    assert!(SessionOpen::decode_canonical(&open.encode()).is_ok());
}

#[test]
fn the_wire_bounds_agree_with_the_geometry_and_the_store() {
    // Constants set independently in different crates that must hold a fixed
    // relationship. Each of these was checked by hand once; asserting them is
    // what keeps the next change from quietly breaking one.
    assert_eq!(
        MAX_PROOF_BYTES,
        replica::content::MAX_PROOF_BYTES,
        "one sidecar ceiling, not a wire one and a storage one"
    );
    assert!(
        bounds::MAX_CHUNK_FRAME_BYTES > replica::content::max_ciphertext_len() + MAX_PROOF_BYTES,
        "a chunk frame must carry a maximal chunk and its proof"
    );
    assert!(
        MAX_WANTED_CHUNKS as u64 <= replica::content::MAX_CHUNK_COUNT as u64,
        "a request cannot name more chunks than any content can have"
    );
    assert_eq!(
        runtime::contact::MAX_CHUNK,
        replica::content::CHUNK_PLAINTEXT_LEN as usize,
        "Contact's body chunk and the content chunk are one number by intent, \
         not by coincidence"
    );
}
