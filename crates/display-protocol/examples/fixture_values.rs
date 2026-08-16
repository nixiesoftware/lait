use display_protocol::auth::{
    authenticate_request, derive_asset_id, derive_program_item_id, request_transcript, sha256,
    RequestContext, RequestMethod, RequestRoute,
};
use display_protocol::ids::{
    Challenge, CoordinatorFingerprint, DisplayAssignmentId, DisplayDeviceId, DisplayPairingId,
    DisplayProgramId, ProgramRevision, ProofKey, ReceiverNonce,
};
use display_protocol::pairing::{authenticate_pairing_complete, confirmation_phrase};
use display_protocol::program::{
    canonical_program_revision, DisplayAsset, DisplayAssetMediaType, DisplayPlayback,
    DisplayProgram, DisplayProgramItem, DisplayScene, DisplaySyncMode, DisplaySyncTarget,
    FreshnessPolicy, ProgramCycle, SourceState, StaleAction,
};
use display_protocol::PROTOCOL_MAJOR;
use serde_json::json;
use std::fmt::Write as _;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let identifier_key = [0x42; 32];
    let assignment = DisplayAssignmentId::parse("00112233445566778899aabbccddeeff")?;
    let item = derive_program_item_id(&identifier_key, &assignment, "welcome-frame")?;
    let asset_digest = sha256(b"fixture png bytes")?;
    let asset = derive_asset_id(
        &identifier_key,
        &assignment,
        DisplayAssetMediaType::ImagePng,
        17,
        &asset_digest,
        Some(1920),
        Some(1080),
    )?;
    let mut program = DisplayProgram {
        protocol_major: PROTOCOL_MAJOR,
        assignment,
        program: DisplayProgramId::parse("102132435465768798a9bacbdcedfe0f")?,
        revision: ProgramRevision::parse("0".repeat(64))?,
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
                    id: asset,
                    media_type: DisplayAssetMediaType::ImagePng,
                    encoded_len: 17,
                    sha256: asset_digest,
                    width: Some(1920),
                    height: Some(1080),
                },
            },
            spoken_summary: Some("Astrolabe receiver fixture".to_owned()),
        }],
    };
    program.revision = canonical_program_revision(&program)?;

    let device = DisplayDeviceId::parse("ffeeddccbbaa99887766554433221100")?;
    let challenge = Challenge::parse("1".repeat(64))?;
    let proof_key = ProofKey::parse("0".repeat(64))?;
    let body_sha256 = sha256(&[])?;
    let context = RequestContext {
        protocol_major: PROTOCOL_MAJOR,
        method: RequestMethod::Get,
        route: RequestRoute::ProgramChanges,
        device: &device,
        assignment: Some(&program.assignment),
        program: Some(&program.program),
        revision: Some(&program.revision),
        current_item: Some(&program.items[0].id),
        elapsed_ms: Some(500),
        wait_ms: Some(25_000),
        asset: None,
        range: None,
        challenge: &challenge,
        body_sha256: &body_sha256,
    };
    let transcript = request_transcript(&context)?;
    let tag = authenticate_request(&proof_key, &context)?;

    let pairing = DisplayPairingId::parse("3".repeat(32))?;
    let enrollment_challenge = Challenge::parse("4".repeat(64))?;
    let pairing_tag = authenticate_pairing_complete(
        &ProofKey::parse("2".repeat(64))?,
        &pairing,
        &device,
        &enrollment_challenge,
    )?;
    let fingerprint = CoordinatorFingerprint::parse("6".repeat(64))?;
    let phrase = confirmation_phrase(
        &fingerprint,
        &DisplayPairingId::parse("7".repeat(32))?,
        &ReceiverNonce::parse("8".repeat(64))?,
    )?;

    let output = json!({
        "schema": "astrolabe.display.conformance.v1",
        "protocol_major": PROTOCOL_MAJOR,
        "fixture_only_keys": {
            "identifier_key_hex": "42".repeat(32),
            "proof_key_hex": proof_key,
        },
        "program": program,
        "program_changes_request": {
            "method": "GET",
            "route": "program_changes",
            "device": device,
            "challenge": challenge,
            "body_sha256": body_sha256,
            "elapsed_ms": 500,
            "wait_ms": 25000,
            "transcript_hex": hex(&transcript),
            "authentication_tag": tag,
        },
        "pairing_complete": {
            "pairing": pairing,
            "device": "ffeeddccbbaa99887766554433221100",
            "challenge": enrollment_challenge,
            "proof_key_hex": "2".repeat(64),
            "authentication_tag": pairing_tag,
        },
        "confirmation_phrase": {
            "fingerprint": fingerprint,
            "pairing": "7".repeat(32),
            "receiver_nonce": "8".repeat(64),
            "words": phrase,
        },
        "negative_cases": [
            {"name": "unknown_protocol_major", "expected": "unsupported"},
            {"name": "unknown_required_field", "expected": "invalid_request"},
            {"name": "forged_program_revision", "expected": "integrity"},
            {"name": "wrong_request_tag", "expected": "authentication_failed"},
            {"name": "replayed_challenge", "expected": "challenge_consumed"},
            {"name": "asset_digest_mismatch", "expected": "asset_digest"},
            {"name": "asset_length_mismatch", "expected": "asset_length"},
            {"name": "oversized_asset", "expected": "bound_exceeded"},
            {"name": "external_asset_url", "expected": "invalid_request"},
            {"name": "receiver_world_coordinate", "expected": "invalid_request"}
        ]
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
