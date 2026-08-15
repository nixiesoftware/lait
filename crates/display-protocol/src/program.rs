//! Complete snapshot and bounded playback semantics.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bounds::{
    LONG_POLL_STALE_MARGIN_MS, MAX_ASSET_BYTES, MAX_FRAME_HEIGHT, MAX_FRAME_PIXELS,
    MAX_FRAME_WIDTH, MAX_ITEM_DURATION_MS, MAX_LONG_POLL_WAIT_MS, MAX_PARTIAL_REASONS,
    MAX_PROGRAM_ITEMS, MAX_STAGING_HORIZON_MS, MAX_STALE_AFTER_MS, MAX_SUMMARY_BYTES,
    MAX_SYNC_GROUP_BYTES, MIN_ITEM_DURATION_MS, MIN_STALE_AFTER_MS,
};
use crate::ids::{
    encode_hex, DisplayAssetId, DisplayAssignmentId, DisplayProgramId, DisplayProgramItemId,
    ProgramRevision, Sha256Digest,
};
use crate::wire::Transcript;
use crate::{Refusal, PROTOCOL_MAJOR};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayPartialReason {
    ProvisionalData,
    CorruptRecords,
    IncompleteProjection,
    DegradedSource,
}

impl DisplayPartialReason {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::ProvisionalData => "provisional_data",
            Self::CorruptRecords => "corrupt_records",
            Self::IncompleteProjection => "incomplete_projection",
            Self::DegradedSource => "degraded_source",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceState {
    Current,
    Partial { reasons: Vec<DisplayPartialReason> },
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramCycle {
    HoldLast,
    Loop,
    PollAtEnd,
    BlankAtEnd,
}

impl ProgramCycle {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::HoldLast => "hold_last",
            Self::Loop => "loop",
            Self::PollAtEnd => "poll_at_end",
            Self::BlankAtEnd => "blank_at_end",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleAction {
    KeepWithNativeBanner,
    Blank,
}

impl StaleAction {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::KeepWithNativeBanner => "keep_with_native_banner",
            Self::Blank => "blank",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshnessPolicy {
    pub stale_after_ms: u32,
    pub on_stale: StaleAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlankReason {
    Unassigned,
    HostUnavailable,
    SourceUnavailable,
    Unsupported,
    Revoked,
    ProgramEnded,
}

impl BlankReason {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Unassigned => "unassigned",
            Self::HostUnavailable => "host_unavailable",
            Self::SourceUnavailable => "source_unavailable",
            Self::Unsupported => "unsupported",
            Self::Revoked => "revoked",
            Self::ProgramEnded => "program_ended",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayAssetMediaType {
    ImagePng,
    ImageJpeg,
    ImageWebp,
    HlsManifest,
    DashManifest,
}

impl DisplayAssetMediaType {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::ImagePng => "image_png",
            Self::ImageJpeg => "image_jpeg",
            Self::ImageWebp => "image_webp",
            Self::HlsManifest => "hls_manifest",
            Self::DashManifest => "dash_manifest",
        }
    }

    pub const fn is_image(self) -> bool {
        matches!(self, Self::ImagePng | Self::ImageJpeg | Self::ImageWebp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaProtocol {
    Hls,
    Dash,
}

impl MediaProtocol {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Hls => "hls",
            Self::Dash => "dash",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayAsset {
    pub id: DisplayAssetId,
    pub media_type: DisplayAssetMediaType,
    pub encoded_len: u32,
    pub sha256: Sha256Digest,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayScene {
    Frame {
        asset: DisplayAsset,
    },
    Media {
        manifest: DisplayAsset,
        protocol: MediaProtocol,
        live: bool,
    },
    Blank {
        reason: BlankReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayProgramItem {
    pub id: DisplayProgramItemId,
    pub duration_ms: Option<u32>,
    pub source_state: SourceState,
    pub scene: DisplayScene,
    pub spoken_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayPlayback {
    pub current_index: u16,
    pub elapsed_ms: u32,
    pub cycle: ProgramCycle,
    pub sync: Option<DisplaySyncTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplaySyncMode {
    StayInSync,
    Positional,
}

impl DisplaySyncMode {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::StayInSync => "stay_in_sync",
            Self::Positional => "positional",
        }
    }
}

/// A correction target sampled on the coordinator's shared time base.
///
/// It is a target, never a playback command. Receivers retain monotonic-clock
/// discipline and apply the best correction their declared tier supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaySyncTarget {
    pub group: String,
    pub mode: DisplaySyncMode,
    pub sampled_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayProgram {
    pub protocol_major: u32,
    pub assignment: DisplayAssignmentId,
    pub program: DisplayProgramId,
    pub revision: ProgramRevision,
    pub program_state: SourceState,
    pub freshness: FreshnessPolicy,
    pub playback: DisplayPlayback,
    pub items: Vec<DisplayProgramItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetReason {
    UnknownRevision,
    AssignmentChanged,
    ServerRestart,
    CursorCorrection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProgramChange {
    Snapshot {
        program: DisplayProgram,
    },
    NoChange {
        revision: ProgramRevision,
        playback: DisplayPlayback,
    },
    Reset {
        reason: ResetReason,
    },
    Unassigned,
    Revoked,
    RePair,
}

fn validate_bounded_text(text: &str, maximum: usize, name: &'static str) -> Result<(), Refusal> {
    if text.is_empty() || text.len() > maximum || text.chars().any(char::is_control) {
        return Err(Refusal::BoundExceeded(name));
    }
    Ok(())
}

pub fn validate_sync_group(group: &str) -> Result<(), Refusal> {
    if group.is_empty()
        || group.len() > MAX_SYNC_GROUP_BYTES
        || !group.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(Refusal::InvalidIdentifier("sync group"));
    }
    Ok(())
}

fn validate_sync_target(target: &DisplaySyncTarget) -> Result<(), Refusal> {
    validate_sync_group(&target.group)?;
    if target.sampled_at_unix_ms == 0 {
        return Err(Refusal::InvalidShape("sync target shared time"));
    }
    Ok(())
}

fn validate_source_state(state: &SourceState) -> Result<(), Refusal> {
    if let SourceState::Partial { reasons } = state {
        if reasons.is_empty() || reasons.len() > MAX_PARTIAL_REASONS {
            return Err(Refusal::InvalidShape("partial source state"));
        }
        let unique: BTreeSet<_> = reasons.iter().map(|reason| reason.wire_name()).collect();
        if unique.len() != reasons.len()
            || unique
                .iter()
                .copied()
                .ne(reasons.iter().map(|reason| reason.wire_name()))
        {
            return Err(Refusal::InvalidShape(
                "partial reasons must be sorted and unique",
            ));
        }
    }
    Ok(())
}

pub fn validate_asset(asset: &DisplayAsset) -> Result<(), Refusal> {
    if asset.encoded_len == 0 || asset.encoded_len > MAX_ASSET_BYTES {
        return Err(Refusal::BoundExceeded("asset encoded length"));
    }
    if asset.media_type.is_image() {
        let (Some(width), Some(height)) = (asset.width, asset.height) else {
            return Err(Refusal::InvalidShape("image dimensions"));
        };
        if width == 0 || width > MAX_FRAME_WIDTH || height == 0 || height > MAX_FRAME_HEIGHT {
            return Err(Refusal::BoundExceeded("image dimensions"));
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(Refusal::BoundExceeded("decoded image pixels"))?;
        if pixels > MAX_FRAME_PIXELS {
            return Err(Refusal::BoundExceeded("decoded image pixels"));
        }
    } else if asset.width.is_some() || asset.height.is_some() {
        return Err(Refusal::InvalidShape("manifest dimensions"));
    }
    Ok(())
}

fn validate_scene(scene: &DisplayScene) -> Result<(), Refusal> {
    match scene {
        DisplayScene::Frame { asset } => {
            if !asset.media_type.is_image() {
                return Err(Refusal::InvalidShape("frame asset media type"));
            }
            validate_asset(asset)
        }
        DisplayScene::Media {
            manifest, protocol, ..
        } => {
            let matches = matches!(
                (protocol, manifest.media_type),
                (MediaProtocol::Hls, DisplayAssetMediaType::HlsManifest)
                    | (MediaProtocol::Dash, DisplayAssetMediaType::DashManifest)
            );
            if !matches {
                return Err(Refusal::InvalidShape("media manifest protocol"));
            }
            validate_asset(manifest)
        }
        DisplayScene::Blank { .. } => Ok(()),
    }
}

fn validate_program_shape(program: &DisplayProgram) -> Result<(), Refusal> {
    if program.protocol_major != PROTOCOL_MAJOR {
        return Err(Refusal::Unsupported("protocol major"));
    }
    if program.items.is_empty() || program.items.len() > MAX_PROGRAM_ITEMS {
        return Err(Refusal::BoundExceeded("program item count"));
    }
    if program.freshness.stale_after_ms < MIN_STALE_AFTER_MS
        || program.freshness.stale_after_ms > MAX_STALE_AFTER_MS
    {
        return Err(Refusal::BoundExceeded("stale interval"));
    }
    let poll_and_margin = MAX_LONG_POLL_WAIT_MS
        .checked_add(LONG_POLL_STALE_MARGIN_MS)
        .ok_or(Refusal::BoundExceeded("long-poll margin"))?;
    if program.freshness.stale_after_ms <= poll_and_margin {
        return Err(Refusal::InvalidShape("stale interval margin"));
    }
    validate_source_state(&program.program_state)?;

    let current = usize::from(program.playback.current_index);
    if current >= program.items.len() {
        return Err(Refusal::InvalidShape("playback current index"));
    }
    if let Some(target) = &program.playback.sync {
        validate_sync_target(target)?;
    }

    let mut item_ids = BTreeSet::new();
    let mut horizon = 0_u32;
    for (index, item) in program.items.iter().enumerate() {
        if !item_ids.insert(item.id.as_str()) {
            return Err(Refusal::InvalidShape("duplicate program item"));
        }
        validate_source_state(&item.source_state)?;
        validate_scene(&item.scene)?;
        if let Some(summary) = item.spoken_summary.as_deref() {
            validate_bounded_text(summary, MAX_SUMMARY_BYTES, "spoken summary")?;
        }

        match item.duration_ms {
            Some(duration) => {
                if !(MIN_ITEM_DURATION_MS..=MAX_ITEM_DURATION_MS).contains(&duration) {
                    return Err(Refusal::BoundExceeded("item duration"));
                }
                horizon = horizon
                    .checked_add(duration)
                    .ok_or(Refusal::BoundExceeded("staging horizon"))?;
            }
            None => {
                let last = index
                    .checked_add(1)
                    .is_some_and(|position| position == program.items.len());
                if !last || program.playback.cycle != ProgramCycle::HoldLast {
                    return Err(Refusal::InvalidShape("open-ended item"));
                }
            }
        }
    }

    if horizon > MAX_STAGING_HORIZON_MS {
        return Err(Refusal::BoundExceeded("staging horizon"));
    }
    let current_item = program
        .items
        .get(current)
        .ok_or(Refusal::InvalidShape("playback current item"))?;
    match current_item.duration_ms {
        Some(duration) if program.playback.elapsed_ms >= duration => {
            return Err(Refusal::InvalidShape("playback elapsed position"));
        }
        _ => {}
    }
    Ok(())
}

pub fn validate_program(program: &DisplayProgram) -> Result<(), Refusal> {
    validate_program_shape(program)?;
    let expected = canonical_program_revision(program)?;
    if expected != program.revision {
        return Err(Refusal::Integrity("program revision"));
    }
    Ok(())
}

fn encode_source_state(transcript: &mut Transcript, state: &SourceState) -> Result<(), Refusal> {
    match state {
        SourceState::Current => transcript.text("current"),
        SourceState::Unavailable => transcript.text("unavailable"),
        SourceState::Partial { reasons } => {
            transcript.text("partial")?;
            let count = u32::try_from(reasons.len())
                .map_err(|_| Refusal::BoundExceeded("partial reason count"))?;
            transcript.u32(count)?;
            for reason in reasons {
                transcript.text(reason.wire_name())?;
            }
            Ok(())
        }
    }
}

fn encode_asset(transcript: &mut Transcript, asset: &DisplayAsset) -> Result<(), Refusal> {
    transcript.text(asset.media_type.wire_name())?;
    transcript.u32(asset.encoded_len)?;
    transcript.text(asset.sha256.as_str())?;
    transcript.optional_u32(asset.width)?;
    transcript.optional_u32(asset.height)
}

pub fn program_semantics_transcript(program: &DisplayProgram) -> Result<Vec<u8>, Refusal> {
    validate_program_shape(program)?;
    let mut transcript = Transcript::new(b"astrolabe-display/program-semantics/v2")?;
    transcript.u32(program.protocol_major)?;
    transcript.text(program.assignment.as_str())?;
    transcript.text(program.program.as_str())?;
    encode_source_state(&mut transcript, &program.program_state)?;
    transcript.u32(program.freshness.stale_after_ms)?;
    transcript.text(program.freshness.on_stale.wire_name())?;
    transcript.text(program.playback.cycle.wire_name())?;
    match &program.playback.sync {
        Some(target) => {
            transcript.boolean(true)?;
            transcript.text(&target.group)?;
            transcript.text(target.mode.wire_name())?;
        }
        None => transcript.boolean(false)?,
    }
    let count = u32::try_from(program.items.len())
        .map_err(|_| Refusal::BoundExceeded("program item count"))?;
    transcript.u32(count)?;
    for item in &program.items {
        transcript.text(item.id.as_str())?;
        transcript.optional_u32(item.duration_ms)?;
        encode_source_state(&mut transcript, &item.source_state)?;
        match &item.scene {
            DisplayScene::Frame { asset } => {
                transcript.text("frame")?;
                encode_asset(&mut transcript, asset)?;
            }
            DisplayScene::Media {
                manifest,
                protocol,
                live,
            } => {
                transcript.text("media")?;
                encode_asset(&mut transcript, manifest)?;
                transcript.text(protocol.wire_name())?;
                transcript.boolean(*live)?;
            }
            DisplayScene::Blank { reason } => {
                transcript.text("blank")?;
                transcript.text(reason.wire_name())?;
            }
        }
        transcript.optional_text(item.spoken_summary.as_deref())?;
    }
    Ok(transcript.finish())
}

pub fn canonical_program_revision(program: &DisplayProgram) -> Result<ProgramRevision, Refusal> {
    let bytes = program_semantics_transcript(program)?;
    let digest = Sha256::digest(bytes);
    ProgramRevision::parse(encode_hex(&digest))
}
