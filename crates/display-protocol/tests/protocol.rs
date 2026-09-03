use display_protocol::auth::{
    authenticate_request, derive_asset_id, derive_program_item_id, request_transcript, sha256,
    verify_request, AssetRange, RequestContext, RequestMethod, RequestRoute,
};
use display_protocol::bounds::{MAX_ASSET_BYTES, MAX_STAGED_BYTES};
use display_protocol::ids::{
    AuthenticationTag, Challenge, CoordinatorFingerprint, CoordinatorProfile, DisplayAssignmentId,
    DisplayDeviceId, DisplayPairingId, DisplayProgramId, DisplayProgramItemId, ProgramRevision,
    ProofKey, ReceiverNonce,
};
use display_protocol::pairing::{
    authenticate_pairing_complete, confirmation_phrase, group_rendezvous_code,
    normalize_rendezvous_code, rendezvous_from_code, validate_bootstrap,
    validate_pairing_start_response, CoordinatorTrust, PairingStartResponse, ReceiverBootstrap,
    RENDEZVOUS_CODE_ALPHABET, RENDEZVOUS_CODE_CHARS,
};
use display_protocol::program::{
    canonical_program_revision, validate_program, BlankReason, DisplayAsset, DisplayAssetMediaType,
    DisplayPartialReason, DisplayPlayback, DisplayProgram, DisplayProgramItem, DisplayScene,
    DisplaySyncMode, DisplaySyncTarget, FreshnessPolicy, MediaProtocol, ProgramCycle, SourceState,
    StaleAction,
};
use display_protocol::receiver::{
    validate_capabilities, validate_health, AccessibilityCapabilities, ConnectionState, Fault,
    HealthGranularity, LatencyBucket, LatencyClass, PipelineStats, PlaybackCapabilities,
    PlaybackState, PlaybackTier, ReceiverCapabilities, ReceiverHealth, ReceiverPlatform, SyncClass,
    Viewport,
};
use display_protocol::{Refusal, PROTOCOL_MAJOR};

fn repeated(character: char, count: usize) -> String {
    std::iter::repeat_n(character, count).collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn assignment() -> DisplayAssignmentId {
    DisplayAssignmentId::parse("00112233445566778899aabbccddeeff").unwrap()
}

fn device() -> DisplayDeviceId {
    DisplayDeviceId::parse("ffeeddccbbaa99887766554433221100").unwrap()
}

fn program_id() -> DisplayProgramId {
    DisplayProgramId::parse("102132435465768798a9bacbdcedfe0f").unwrap()
}

fn identifier_key() -> [u8; 32] {
    [0x42; 32]
}

fn fixture_program() -> DisplayProgram {
    let assignment = assignment();
    let item = derive_program_item_id(&identifier_key(), &assignment, "welcome-frame").unwrap();
    let digest = sha256(b"fixture png bytes").unwrap();
    let asset_id = derive_asset_id(
        &identifier_key(),
        &assignment,
        DisplayAssetMediaType::ImagePng,
        17,
        &digest,
        Some(1920),
        Some(1080),
    )
    .unwrap();
    let mut program = DisplayProgram {
        protocol_major: PROTOCOL_MAJOR,
        assignment,
        program: program_id(),
        revision: ProgramRevision::parse(repeated('0', 64)).unwrap(),
        program_state: SourceState::Current,
        freshness: FreshnessPolicy {
            stale_after_ms: 60_000,
            on_stale: StaleAction::KeepWithNativeBanner,
        },
        playback: DisplayPlayback {
            current_index: 0,
            elapsed_ms: 125,
            cycle: ProgramCycle::HoldLast,
            sync: Some(DisplaySyncTarget {
                group: "lobby".into(),
                mode: DisplaySyncMode::Positional,
                sampled_at_unix_ms: 1_786_744_181_000,
            }),
        },
        items: vec![DisplayProgramItem {
            id: item,
            duration_ms: None,
            source_state: SourceState::Current,
            scene: DisplayScene::Frame {
                asset: DisplayAsset {
                    id: asset_id,
                    media_type: DisplayAssetMediaType::ImagePng,
                    encoded_len: 17,
                    sha256: digest,
                    width: Some(1920),
                    height: Some(1080),
                },
            },
            spoken_summary: Some("Astrolabe receiver fixture".to_owned()),
        }],
    };
    program.revision = canonical_program_revision(&program).unwrap();
    program
}

fn fixture_capabilities() -> ReceiverCapabilities {
    ReceiverCapabilities {
        protocol_major: PROTOCOL_MAJOR,
        platform: ReceiverPlatform::Roku,
        build: "astrolabe-roku/0.1.0".to_owned(),
        viewport: Viewport {
            width: 1920,
            height: 1080,
            scale_milli: 1000,
        },
        image_types: vec![
            DisplayAssetMediaType::ImageJpeg,
            DisplayAssetMediaType::ImagePng,
        ],
        max_asset_bytes: MAX_ASSET_BYTES,
        max_staged_bytes: MAX_STAGED_BYTES,
        max_program_items: 16,
        max_staging_horizon_ms: 86_400_000,
        locale: "en-US".to_owned(),
        accessibility: AccessibilityCapabilities {
            native_screen_reader: true,
            spoken_summary: true,
            captions: false,
            audio_description: false,
        },
        playback: PlaybackCapabilities {
            tier: PlaybackTier::Frame,
            sync_class: SyncClass::Boundary,
            rate_control_probed: false,
            latency_class: LatencyClass::Snapshot,
            health_granularity: HealthGranularity::Full,
        },
    }
}

#[test]
fn random_and_derived_identifier_domains_are_exact() {
    assert!(DisplayDeviceId::parse(repeated('a', 32)).is_ok());
    assert!(DisplayDeviceId::parse(repeated('a', 31)).is_err());
    assert!(DisplayDeviceId::parse(repeated('A', 32)).is_err());
    assert!(DisplayDeviceId::parse(repeated('g', 32)).is_err());
    assert!(ProgramRevision::parse(repeated('f', 64)).is_ok());
    assert!(ProgramRevision::parse(repeated('f', 65)).is_err());
}

#[test]
fn secret_debug_output_never_contains_secret_material() {
    let proof = ProofKey::parse(repeated('a', 64)).unwrap();
    assert_eq!(format!("{proof:?}"), "ProofKey([REDACTED])");
}

#[test]
fn a_program_revision_commits_semantics_but_not_cursor_position() {
    let program = fixture_program();
    validate_program(&program).unwrap();

    let mut moved = program.clone();
    moved.playback.elapsed_ms = 200;
    if let Some(sync) = &mut moved.playback.sync {
        sync.sampled_at_unix_ms = sync.sampled_at_unix_ms.saturating_add(1_000);
    }
    assert_eq!(
        canonical_program_revision(&moved).unwrap(),
        program.revision
    );

    let mut changed = program.clone();
    changed.program_state = SourceState::Unavailable;
    assert_ne!(
        canonical_program_revision(&changed).unwrap(),
        program.revision
    );

    let mut regrouped = program.clone();
    if let Some(sync) = &mut regrouped.playback.sync {
        sync.group = "atrium".into();
    }
    assert_ne!(
        canonical_program_revision(&regrouped).unwrap(),
        program.revision
    );
}

#[test]
fn a_forged_program_revision_is_refused() {
    let mut program = fixture_program();
    program.revision = ProgramRevision::parse(repeated('f', 64)).unwrap();
    assert_eq!(
        validate_program(&program),
        Err(Refusal::Integrity("program revision"))
    );
}

#[test]
fn open_ended_items_exist_only_at_hold_last_end() {
    let mut program = fixture_program();
    program.playback.cycle = ProgramCycle::Loop;
    assert_eq!(
        canonical_program_revision(&program),
        Err(Refusal::InvalidShape("open-ended item"))
    );
}

#[test]
fn source_partial_reasons_are_bounded_sorted_and_unique() {
    let mut program = fixture_program();
    program.program_state = SourceState::Partial {
        reasons: vec![
            DisplayPartialReason::IncompleteProjection,
            DisplayPartialReason::CorruptRecords,
        ],
    };
    assert_eq!(
        canonical_program_revision(&program),
        Err(Refusal::InvalidShape(
            "partial reasons must be sorted and unique"
        ))
    );
}

#[test]
fn cross_language_lists_use_wire_string_order() {
    let mut program = fixture_program();
    program.program_state = SourceState::Partial {
        reasons: vec![
            DisplayPartialReason::CorruptRecords,
            DisplayPartialReason::DegradedSource,
            DisplayPartialReason::IncompleteProjection,
            DisplayPartialReason::ProvisionalData,
        ],
    };
    program.revision = canonical_program_revision(&program).expect("wire-sorted partial reasons");
    assert_eq!(validate_program(&program), Ok(()));

    let capabilities = fixture_capabilities();
    assert_eq!(validate_capabilities(&capabilities), Ok(()));
}

#[test]
fn frame_metadata_cannot_smuggle_a_manifest_or_unbounded_decode() {
    let mut program = fixture_program();
    if let DisplayScene::Frame { asset } = &mut program.items[0].scene {
        asset.width = Some(4096);
        asset.height = Some(2161);
    }
    assert_eq!(
        canonical_program_revision(&program),
        Err(Refusal::BoundExceeded("image dimensions"))
    );

    if let DisplayScene::Frame { asset } = &mut program.items[0].scene {
        asset.width = None;
        asset.height = None;
        asset.media_type = DisplayAssetMediaType::HlsManifest;
    }
    assert_eq!(
        canonical_program_revision(&program),
        Err(Refusal::InvalidShape("frame asset media type"))
    );
}

#[test]
fn live_media_manifest_and_protocol_must_match() {
    let mut program = fixture_program();
    let digest = sha256(br#"{"version":1,"resource":"main"}"#).unwrap();
    let manifest = DisplayAsset {
        id: derive_asset_id(
            &identifier_key(),
            &program.assignment,
            DisplayAssetMediaType::MseManifest,
            31,
            &digest,
            None,
            None,
        )
        .unwrap(),
        media_type: DisplayAssetMediaType::MseManifest,
        encoded_len: 31,
        sha256: digest,
        width: None,
        height: None,
    };
    program.items[0].scene = DisplayScene::Media {
        manifest,
        protocol: MediaProtocol::Mse,
        live: true,
    };
    program.revision = canonical_program_revision(&program).unwrap();
    assert_eq!(validate_program(&program), Ok(()));

    if let DisplayScene::Media { protocol, .. } = &mut program.items[0].scene {
        *protocol = MediaProtocol::Hls;
    }
    assert_eq!(
        canonical_program_revision(&program),
        Err(Refusal::InvalidShape("media manifest protocol"))
    );
}

#[test]
fn receiver_capabilities_can_only_reduce_server_bounds() {
    let capabilities = fixture_capabilities();
    validate_capabilities(&capabilities).unwrap();

    let mut oversized = capabilities;
    oversized.max_asset_bytes = MAX_ASSET_BYTES + 1;
    assert_eq!(
        validate_capabilities(&oversized),
        Err(Refusal::BoundExceeded("receiver asset bytes"))
    );
}

#[test]
fn playback_tier_degradation_is_explicit() {
    let mut capabilities = fixture_capabilities();
    capabilities.playback.tier = PlaybackTier::NativeHls;
    capabilities.playback.sync_class = SyncClass::Boundary;
    assert_eq!(
        validate_capabilities(&capabilities),
        Err(Refusal::InvalidShape("native HLS tier"))
    );
}

#[test]
fn request_authentication_commits_every_route_coordinate() {
    let proof_key = ProofKey::parse(repeated('0', 64)).unwrap();
    let challenge = Challenge::parse(repeated('1', 64)).unwrap();
    let body = sha256(&[]).unwrap();
    let program = fixture_program();
    let current_item = &program.items[0].id;
    let context = RequestContext {
        protocol_major: PROTOCOL_MAJOR,
        method: RequestMethod::Get,
        route: RequestRoute::ProgramChanges,
        device: &device(),
        assignment: Some(&program.assignment),
        program: Some(&program.program),
        revision: Some(&program.revision),
        current_item: Some(current_item),
        elapsed_ms: Some(500),
        wait_ms: Some(25_000),
        asset: None,
        range: None,
        challenge: &challenge,
        body_sha256: &body,
    };
    let tag = authenticate_request(&proof_key, &context).unwrap();
    verify_request(&proof_key, &context, &tag).unwrap();

    let altered = RequestContext {
        elapsed_ms: Some(501),
        ..context
    };
    assert_eq!(
        verify_request(&proof_key, &altered, &tag),
        Err(Refusal::Integrity("request authentication tag"))
    );
}

#[test]
fn route_shape_refuses_an_asset_request_without_an_asset() {
    let challenge = Challenge::parse(repeated('1', 64)).unwrap();
    let body = sha256(&[]).unwrap();
    let program = fixture_program();
    let context = RequestContext {
        protocol_major: PROTOCOL_MAJOR,
        method: RequestMethod::Get,
        route: RequestRoute::Asset,
        device: &device(),
        assignment: Some(&program.assignment),
        program: Some(&program.program),
        revision: Some(&program.revision),
        current_item: None,
        elapsed_ms: None,
        wait_ms: None,
        asset: None,
        range: Some(AssetRange {
            start: 0,
            length: 10,
        }),
        challenge: &challenge,
        body_sha256: &body,
    };
    assert_eq!(
        request_transcript(&context),
        Err(Refusal::InvalidShape("request authentication context"))
    );
}

#[test]
fn live_ticket_route_commits_the_current_item_and_manifest() {
    let challenge = Challenge::parse(repeated('1', 64)).unwrap();
    let body = sha256(br#"{"transport":"hls"}"#).unwrap();
    let program = fixture_program();
    let asset = derive_asset_id(
        &identifier_key(),
        &program.assignment,
        DisplayAssetMediaType::HlsManifest,
        1,
        &sha256(b"x").unwrap(),
        None,
        None,
    )
    .unwrap();
    let context = RequestContext {
        protocol_major: PROTOCOL_MAJOR,
        method: RequestMethod::Post,
        route: RequestRoute::LiveTicket,
        device: &device(),
        assignment: Some(&program.assignment),
        program: Some(&program.program),
        revision: Some(&program.revision),
        current_item: Some(&program.items[0].id),
        elapsed_ms: Some(0),
        wait_ms: None,
        asset: Some(&asset),
        range: None,
        challenge: &challenge,
        body_sha256: &body,
    };
    assert!(request_transcript(&context).is_ok());
    assert_eq!(
        request_transcript(&RequestContext {
            current_item: None,
            ..context
        }),
        Err(Refusal::InvalidShape("request authentication context"))
    );
}

#[test]
fn pairing_completion_proof_changes_with_the_enrolled_device() {
    let proof_key = ProofKey::parse(repeated('2', 64)).unwrap();
    let pairing = DisplayPairingId::parse(repeated('3', 32)).unwrap();
    let challenge = Challenge::parse(repeated('4', 64)).unwrap();
    let first = authenticate_pairing_complete(&proof_key, &pairing, &device(), &challenge).unwrap();
    let other_device = DisplayDeviceId::parse(repeated('5', 32)).unwrap();
    let second =
        authenticate_pairing_complete(&proof_key, &pairing, &other_device, &challenge).unwrap();
    assert_ne!(first, second);
}

#[test]
fn a_rendezvous_code_is_read_the_way_a_person_types_it() {
    // Case, spacing and the grouping hyphen are the operator's; the letters
    // that look like digits fold onto them.
    assert_eq!(
        normalize_rendezvous_code(" 7k3q-oi1L ").unwrap(),
        "7K3Q0111"
    );
    assert_eq!(group_rendezvous_code("7k3q0111").unwrap(), "7K3Q-0111");
    assert_eq!(RENDEZVOUS_CODE_ALPHABET.len(), 32);
    for bad in [
        "7K3Q011",
        "7K3Q01111",
        "7K3Q-0U11",
        "",
        "7K3Q 0!11",
        "7K3Q-0Ü1",
    ] {
        assert!(
            matches!(
                normalize_rendezvous_code(bad),
                Err(Refusal::InvalidIdentifier(_))
            ),
            "{bad:?} is not a code"
        );
    }
    // Every symbol of the alphabet is accepted as itself.
    let all: String = RENDEZVOUS_CODE_ALPHABET
        .iter()
        .take(RENDEZVOUS_CODE_CHARS)
        .map(|byte| char::from(*byte))
        .collect();
    assert_eq!(normalize_rendezvous_code(&all).unwrap(), all);
}

#[test]
fn a_rendezvous_id_derives_from_the_code_and_nothing_else() {
    let first = rendezvous_from_code("7K3Q-0111").unwrap();
    // The same code however it was entered is the same rendezvous.
    assert_eq!(rendezvous_from_code(" 7k3q oi1l ").unwrap(), first);
    assert_ne!(rendezvous_from_code("7K3Q-0112").unwrap(), first);
    assert_eq!(first.as_str().len(), 32);
}

#[test]
fn confirmation_phrase_commits_profile_pairing_and_receiver_nonce() {
    let profile = CoordinatorProfile::parse(format!("prf_{}", repeated('6', 26))).unwrap();
    let pairing = DisplayPairingId::parse(repeated('7', 32)).unwrap();
    let nonce = ReceiverNonce::parse(repeated('8', 64)).unwrap();
    let first = confirmation_phrase(&profile, &pairing, &nonce).unwrap();
    assert_eq!(first.len(), 6);

    let other = ReceiverNonce::parse(repeated('9', 64)).unwrap();
    assert_ne!(
        first,
        confirmation_phrase(&profile, &pairing, &other).unwrap()
    );

    // A different identity is different words: the phrase is what a person
    // compares, and it must move with *who*, not with any placement fact.
    let stranger = CoordinatorProfile::parse(format!("prf_{}", repeated('9', 26))).unwrap();
    assert_ne!(
        first,
        confirmation_phrase(&stranger, &pairing, &nonce).unwrap()
    );

    use sha2::Digest as _;
    let certificate = b"bounded bootstrap certificate";
    let digest = sha2::Sha256::digest(certificate);
    let certificate_fingerprint = CoordinatorFingerprint::parse(hex(&digest)).unwrap();
    let encoded = data_encoding::BASE64.encode(certificate);
    let bootstrap = ReceiverBootstrap {
        protocol_major: PROTOCOL_MAJOR,
        trust: CoordinatorTrust::PinnedCertificate {
            origin: "https://192.0.2.10:7443".to_owned(),
            sha256: certificate_fingerprint,
        },
        certificate_pem: Some(format!(
            "-----BEGIN CERTIFICATE-----\n{encoded}\n-----END CERTIFICATE-----\n"
        )),
        rendezvous: None,
    };
    assert_eq!(validate_bootstrap(&bootstrap), Ok(()));
    let mut mismatched = bootstrap;
    mismatched.certificate_pem =
        Some("-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n".to_owned());
    assert_eq!(
        validate_bootstrap(&mismatched),
        Err(Refusal::Integrity("pinned certificate fingerprint"))
    );
}

#[test]
fn confirmation_words_outside_the_frozen_dictionary_are_refused() {
    let response = PairingStartResponse {
        protocol_major: PROTOCOL_MAJOR,
        pairing: DisplayPairingId::parse(repeated('7', 32)).unwrap(),
        expires_in_ms: 60_000,
        confirmation_phrase: vec!["invented".to_owned(); 6],
        coordinator_fingerprint: CoordinatorFingerprint::parse(repeated('6', 64)).unwrap(),
        coordinator_profile: CoordinatorProfile::parse(format!("prf_{}", repeated('6', 26)))
            .unwrap(),
    };
    assert_eq!(
        validate_pairing_start_response(&response),
        Err(Refusal::InvalidShape("confirmation phrase"))
    );
}

#[test]
fn unknown_program_fields_are_not_best_effort_interpreted() {
    let program = fixture_program();
    let mut value = serde_json::to_value(program).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("world".to_owned(), serde_json::json!("forbidden"));
    assert!(serde_json::from_value::<DisplayProgram>(value).is_err());
}

#[test]
fn a_blank_is_a_closed_native_reason_not_a_url_or_message() {
    let scene = DisplayScene::Blank {
        reason: BlankReason::Revoked,
    };
    assert_eq!(
        serde_json::to_value(scene).unwrap(),
        serde_json::json!({"kind": "blank", "reason": "revoked"})
    );
}

#[test]
fn malformed_authentication_tags_are_refused_before_comparison() {
    assert!(AuthenticationTag::parse(repeated('z', 64)).is_err());
}

#[test]
fn language_neutral_fixture_matches_rust_transcripts_and_json() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/v1/conformance.json")).unwrap();
    let program: DisplayProgram =
        serde_json::from_value(fixture.get("program").unwrap().clone()).unwrap();
    validate_program(&program).unwrap();

    let request = fixture.get("program_changes_request").unwrap();
    let proof_key = ProofKey::parse(
        fixture
            .pointer("/fixture_only_keys/proof_key_hex")
            .and_then(serde_json::Value::as_str)
            .unwrap(),
    )
    .unwrap();
    let challenge = Challenge::parse(request.get("challenge").unwrap().as_str().unwrap()).unwrap();
    let body = sha256(&[]).unwrap();
    let context = RequestContext {
        protocol_major: PROTOCOL_MAJOR,
        method: RequestMethod::Get,
        route: RequestRoute::ProgramChanges,
        device: &device(),
        assignment: Some(&program.assignment),
        program: Some(&program.program),
        revision: Some(&program.revision),
        current_item: Some(&program.items[0].id),
        elapsed_ms: Some(500),
        wait_ms: Some(25_000),
        asset: None,
        range: None,
        challenge: &challenge,
        body_sha256: &body,
    };
    assert_eq!(
        hex(&request_transcript(&context).unwrap()),
        request.get("transcript_hex").unwrap().as_str().unwrap()
    );
    assert_eq!(
        authenticate_request(&proof_key, &context).unwrap().as_str(),
        request.get("authentication_tag").unwrap().as_str().unwrap()
    );

    let phrase = fixture.get("confirmation_phrase").unwrap();
    let words = confirmation_phrase(
        &CoordinatorProfile::parse(phrase.get("profile").unwrap().as_str().unwrap()).unwrap(),
        &DisplayPairingId::parse(phrase.get("pairing").unwrap().as_str().unwrap()).unwrap(),
        &ReceiverNonce::parse(phrase.get("receiver_nonce").unwrap().as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(words).unwrap(),
        phrase.get("words").unwrap().clone()
    );

    let code = fixture.get("rendezvous_code").unwrap();
    let entered = code.get("entered").unwrap().as_str().unwrap();
    assert_eq!(
        normalize_rendezvous_code(entered).unwrap(),
        code.get("normalized").unwrap().as_str().unwrap()
    );
    assert_eq!(
        group_rendezvous_code(entered).unwrap(),
        code.get("grouped").unwrap().as_str().unwrap()
    );
    assert_eq!(
        rendezvous_from_code(entered).unwrap().as_str(),
        code.get("rendezvous").unwrap().as_str().unwrap()
    );
}

fn health() -> ReceiverHealth {
    ReceiverHealth {
        protocol_major: PROTOCOL_MAJOR,
        platform: ReceiverPlatform::Roku,
        build: "astrolabe-roku/0.1.2+abcdef12".into(),
        revision: ProgramRevision::parse(repeated('0', 64)).unwrap(),
        current_item: DisplayProgramItemId::parse(repeated('1', 64)).unwrap(),
        elapsed_ms: 1_000,
        last_displayed_asset: None,
        connection: ConnectionState::Online,
        playback: PlaybackState::Displaying,
        last_error: Fault::None,
        staged_items: 1,
        staged_bytes: 0,
        decode_latency: LatencyBucket::Unobserved,
        swap_latency: LatencyBucket::Unobserved,
        drift_residual_ms: 0,
        correction_events: 0,
        pipeline_unobservable: true,
        pipeline: None,
    }
}

/// A receiver that can read its player says so with numbers; one that
/// cannot omits the field and the report is still whole. The state word is
/// the player's own and is bounded like every other string on this plane.
#[test]
fn a_health_report_carries_the_player_pipeline_when_the_receiver_can_read_it() {
    let mut report = health();
    validate_health(&report).unwrap();
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(
        !encoded.contains("\"pipeline\""),
        "absent means absent, not null"
    );
    let decoded: ReceiverHealth = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, report);

    report.pipeline_unobservable = false;
    report.pipeline = Some(PipelineStats {
        state: "playing".into(),
        position_ms: 12_345,
        frames_rendered: 400,
        frames_dropped: 3,
        frames_repeated: 1,
        stream_errors: 0,
        segment_sequence: Some(282),
        buffering: false,
    });
    validate_health(&report).unwrap();
    let encoded = serde_json::to_string(&report).unwrap();
    let decoded: ReceiverHealth = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.pipeline.as_ref().unwrap().frames_dropped, 3);

    let mut shouting = report.clone();
    shouting.pipeline.as_mut().unwrap().state = "Playing!".into();
    assert!(validate_health(&shouting).is_err());
    let mut long = report;
    long.pipeline.as_mut().unwrap().state = repeated('a', 25);
    assert!(validate_health(&long).is_err());
}
