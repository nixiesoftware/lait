//! Package-owned display-surface compilation.

use std::collections::BTreeMap;

use anyhow::{anyhow, Context, Result};
use display_protocol::auth::{derive_asset_id, derive_program_item_id, sha256};
use display_protocol::ids::{
    DisplayAssetId, DisplayAssignmentId, DisplayProgramId, ProgramRevision,
};
use display_protocol::program::{
    canonical_program_revision, validate_program, BlankReason, DisplayAsset, DisplayAssetMediaType,
    DisplayPlayback, DisplayProgram, DisplayProgramItem, DisplayScene, DisplaySyncMode,
    DisplaySyncTarget, FreshnessPolicy, ProgramCycle, SourceState,
};
use display_protocol::receiver::PlaybackTier;
use world_interface::display::{
    DisplayAssessment, DisplayPartialReason, DisplayProjection, FrameMediaType, RenderedScene,
};

pub struct CompiledProgram {
    pub program: DisplayProgram,
    /// Package-selected deadline after which the World projection itself may
    /// change without a write. Kept separate from the effective refresh so a
    /// sync-item boundary can realign the cheap playback cursor without
    /// rendering every frame through the World runner again.
    pub source_refresh_after_ms: Option<u32>,
    /// Package-selected boundary after which the same Query must be projected
    /// again, or the next synchronized playback boundary, whichever is first.
    pub refresh_after_ms: Option<u32>,
    assets: BTreeMap<DisplayAssetId, Vec<u8>>,
    media_resources: BTreeMap<DisplayAssetId, String>,
}

impl CompiledProgram {
    pub fn asset(&self, id: &DisplayAssetId) -> Option<&[u8]> {
        self.assets.get(id).map(Vec::as_slice)
    }

    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }

    pub fn assets(&self) -> impl Iterator<Item = (&DisplayAssetId, &[u8])> {
        self.assets
            .iter()
            .map(|(identifier, bytes)| (identifier, bytes.as_slice()))
    }

    pub fn media_resource(&self, manifest: &DisplayAssetId) -> Option<&str> {
        self.media_resources.get(manifest).map(String::as_str)
    }
}

pub struct ProgramCompiler {
    identifier_key: [u8; 32],
}

pub struct PlaybackAlignment {
    pub group: String,
    pub mode: DisplaySyncMode,
    pub epoch_unix_ms: u64,
    pub sampled_at_unix_ms: u64,
    /// Positive values advance the logical cursor to compensate for a
    /// receiver's measured presentation latency.
    pub static_delay_ms: i32,
}

impl ProgramCompiler {
    pub fn new(identifier_key: [u8; 32]) -> Result<Self> {
        if identifier_key == [0; 32] {
            return Err(anyhow!("display identifier key must be non-zero"));
        }
        Ok(Self { identifier_key })
    }

    pub fn compile(
        &self,
        assignment: &DisplayAssignmentId,
        program: &DisplayProgramId,
        freshness: FreshnessPolicy,
        projection: DisplayProjection,
        alignment: Option<&PlaybackAlignment>,
        playback_tier: PlaybackTier,
    ) -> Result<CompiledProgram> {
        let source_refresh_after_ms = projection.program.refresh_after_ms;
        let mut refresh_after_ms = source_refresh_after_ms;
        let mut assets = BTreeMap::new();
        let mut media_resources = BTreeMap::new();
        let mut items = Vec::with_capacity(projection.program.items.len());
        for item in projection.program.items {
            let id = derive_program_item_id(&self.identifier_key, assignment, &item.id)
                .context("derive receiver program item id")?;
            let scene = match item.scene {
                RenderedScene::Frame(frame) => {
                    let media_type = match frame.media_type {
                        FrameMediaType::Png => DisplayAssetMediaType::ImagePng,
                        FrameMediaType::Jpeg => DisplayAssetMediaType::ImageJpeg,
                        FrameMediaType::WebP => DisplayAssetMediaType::ImageWebp,
                    };
                    let encoded_len = u32::try_from(frame.bytes.len())
                        .map_err(|_| anyhow!("rendered frame is too large"))?;
                    let digest = sha256(&frame.bytes).context("digest rendered frame")?;
                    let asset_id = derive_asset_id(
                        &self.identifier_key,
                        assignment,
                        media_type,
                        encoded_len,
                        &digest,
                        Some(frame.width),
                        Some(frame.height),
                    )
                    .context("derive receiver asset id")?;
                    let asset = DisplayAsset {
                        id: asset_id.clone(),
                        media_type,
                        encoded_len,
                        sha256: digest,
                        width: Some(frame.width),
                        height: Some(frame.height),
                    };
                    assets.insert(asset_id, frame.bytes);
                    DisplayScene::Frame { asset }
                }
                RenderedScene::Media(media) => {
                    match media_scene(&self.identifier_key, assignment, media, playback_tier)? {
                        Some((scene, asset_id, bytes, resource)) => {
                            assets.insert(asset_id.clone(), bytes);
                            media_resources.insert(asset_id, resource);
                            scene
                        }
                        None => DisplayScene::Blank {
                            reason: BlankReason::Unsupported,
                        },
                    }
                }
                // Resolved by the coordinator before anything is compiled; one
                // reaching here is a bug, and is said rather than blanked.
                RenderedScene::StoredFrame(_) => {
                    return Err(anyhow!("stored frame reached the compiler unresolved"));
                }
                RenderedScene::Blank(reason) => DisplayScene::Blank {
                    reason: match reason {
                        world_interface::display::BlankReason::SourceUnavailable => {
                            BlankReason::SourceUnavailable
                        }
                        world_interface::display::BlankReason::Unsupported => {
                            BlankReason::Unsupported
                        }
                        world_interface::display::BlankReason::ProgramEnded => {
                            BlankReason::ProgramEnded
                        }
                    },
                },
            };
            items.push(DisplayProgramItem {
                id,
                duration_ms: item.duration_ms,
                source_state: source_state(&item.assessment),
                scene,
                spoken_summary: item.spoken_summary,
            });
        }
        let placeholder = ProgramRevision::parse("0".repeat(64))
            .context("construct receiver program revision placeholder")?;
        let program_cycle = cycle(projection.program.cycle);
        let (playback, alignment_refresh) = if let Some(alignment) = alignment {
            aligned_playback(&items, program_cycle, alignment)?
        } else {
            (
                DisplayPlayback {
                    current_index: 0,
                    elapsed_ms: 0,
                    cycle: program_cycle,
                    sync: None,
                },
                None,
            )
        };
        if let Some(alignment_refresh) = alignment_refresh {
            refresh_after_ms = Some(
                refresh_after_ms
                    .map_or(alignment_refresh, |package| package.min(alignment_refresh)),
            );
        }
        let mut wire = DisplayProgram {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            assignment: assignment.clone(),
            program: program.clone(),
            revision: placeholder,
            program_state: source_state(&projection.assessment),
            freshness,
            playback,
            items,
        };
        wire.revision = canonical_program_revision(&wire).context("revise receiver program")?;
        validate_program(&wire).context("validate compiled receiver program")?;
        Ok(CompiledProgram {
            program: wire,
            source_refresh_after_ms,
            refresh_after_ms,
            assets,
            media_resources,
        })
    }

    /// Compile a program the coordinator has already composed into one HLS
    /// stream: a single looping media item pointing at `program_resource`, so
    /// the receiver plays the whole program on one player and never switches
    /// surfaces. The stills and clips live inside the stream; the receiver
    /// stages nothing per item.
    /// Compile the whole-program stream: one open-ended media item the
    /// receiver holds for the life of the assignment.
    ///
    /// The item carries no duration and the program holds its last item, so
    /// the revision is a function of the assignment, the program id, the
    /// freshness policy and the stream resource — none of which a program edit
    /// touches. It used to carry the program's total length, which made every
    /// edit that changed the length a new revision: the receiver re-staged,
    /// minted a new ticket and reloaded its player for a stream whose URL had
    /// no reason to change. The stream itself is endless (a live playlist
    /// with no `EXT-X-ENDLIST`), so a length on the wire was never true.
    pub fn compile_stream(
        &self,
        assignment: &DisplayAssignmentId,
        program: &DisplayProgramId,
        freshness: FreshnessPolicy,
        program_resource: &str,
        refresh_after_ms: Option<u32>,
    ) -> Result<CompiledProgram> {
        let media_type = DisplayAssetMediaType::HlsManifest;
        let bytes = serde_json::to_vec(&LiveManifest {
            version: 1,
            origin: "stored",
            resource: program_resource,
        })
        .context("encode program stream manifest")?;
        let encoded_len = u32::try_from(bytes.len()).context("program manifest is too large")?;
        let digest = sha256(&bytes).context("digest program stream manifest")?;
        let asset_id = derive_asset_id(
            &self.identifier_key,
            assignment,
            media_type,
            encoded_len,
            &digest,
            None,
            None,
        )
        .context("derive program stream manifest id")?;
        let manifest = DisplayAsset {
            id: asset_id.clone(),
            media_type,
            encoded_len,
            sha256: digest,
            width: None,
            height: None,
        };
        let item_id = derive_program_item_id(&self.identifier_key, assignment, "program-stream")
            .context("derive program stream item id")?;
        let item = DisplayProgramItem {
            id: item_id,
            // Open-ended: the stream never ends, and its length is not a fact
            // about the assignment. The protocol allows this only on the last
            // item of a program that holds its last, which this is.
            duration_ms: None,
            source_state: SourceState::Current,
            scene: DisplayScene::Media {
                manifest,
                protocol: display_protocol::program::MediaProtocol::Hls,
                live: false,
            },
            spoken_summary: None,
        };
        let mut assets = BTreeMap::new();
        assets.insert(asset_id.clone(), bytes);
        let mut media_resources = BTreeMap::new();
        media_resources.insert(asset_id, program_resource.to_string());
        let mut wire = DisplayProgram {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            assignment: assignment.clone(),
            program: program.clone(),
            revision: ProgramRevision::parse("0".repeat(64))
                .context("construct program revision placeholder")?,
            program_state: SourceState::Current,
            freshness,
            playback: DisplayPlayback {
                current_index: 0,
                elapsed_ms: 0,
                cycle: ProgramCycle::HoldLast,
                sync: None,
            },
            items: vec![item],
        };
        wire.revision = canonical_program_revision(&wire).context("revise program stream")?;
        validate_program(&wire).context("validate program stream")?;
        Ok(CompiledProgram {
            program: wire,
            source_refresh_after_ms: refresh_after_ms,
            refresh_after_ms,
            assets,
            media_resources,
        })
    }
}

#[derive(serde::Serialize)]
struct LiveManifest<'a> {
    version: u16,
    /// `live` or `stored`. Carried so the two namespaces cannot collide into
    /// one `derive_asset_id`, and so the bytes say which plane resolves them.
    origin: &'a str,
    resource: &'a str,
}

fn media_scene(
    identifier_key: &[u8; 32],
    assignment: &DisplayAssignmentId,
    media: world_interface::display::RenderedMedia,
    playback_tier: PlaybackTier,
) -> Result<Option<(DisplayScene, DisplayAssetId, Vec<u8>, String)>> {
    // The tier decides the transport for both origins. It used to be consulted
    // only for live scenes, so a stored one reached a Frame-tier receiver as
    // media it could not draw and was never told it could not draw — a screen
    // holding its previous item while health reported the revision delivered.
    let (protocol, media_type) = match playback_tier {
        PlaybackTier::MseLive => (
            display_protocol::program::MediaProtocol::Mse,
            DisplayAssetMediaType::MseManifest,
        ),
        PlaybackTier::NativeHls | PlaybackTier::NativeFull => (
            display_protocol::program::MediaProtocol::Hls,
            DisplayAssetMediaType::HlsManifest,
        ),
        PlaybackTier::Frame => return Ok(None),
    };
    let live = media.origin.is_live();
    let resource = match &media.origin {
        world_interface::display::MediaOrigin::Live(rendition) => rendition.as_str().to_string(),
        world_interface::display::MediaOrigin::Stored(content) => {
            data_encoding::HEXLOWER.encode(content.as_bytes())
        }
    };
    let bytes = serde_json::to_vec(&LiveManifest {
        version: 1,
        origin: if live { "live" } else { "stored" },
        resource: &resource,
    })
    .context("encode live display manifest")?;
    let encoded_len = u32::try_from(bytes.len()).context("live manifest is too large")?;
    let digest = sha256(&bytes).context("digest live display manifest")?;
    let asset_id = derive_asset_id(
        identifier_key,
        assignment,
        media_type,
        encoded_len,
        &digest,
        None,
        None,
    )
    .context("derive live display manifest id")?;
    let manifest = DisplayAsset {
        id: asset_id.clone(),
        media_type,
        encoded_len,
        sha256: digest,
        width: None,
        height: None,
    };
    Ok(Some((
        DisplayScene::Media {
            manifest,
            protocol,
            live,
        },
        asset_id,
        bytes,
        resource,
    )))
}

pub(super) fn aligned_playback(
    items: &[DisplayProgramItem],
    cycle: ProgramCycle,
    alignment: &PlaybackAlignment,
) -> Result<(DisplayPlayback, Option<u32>)> {
    let elapsed = i128::from(alignment.sampled_at_unix_ms)
        .checked_sub(i128::from(alignment.epoch_unix_ms))
        .and_then(|value| value.checked_add(i128::from(alignment.static_delay_ms)))
        .ok_or_else(|| anyhow!("display sync position overflowed"))?
        .max(0);
    let mut position = u64::try_from(elapsed).context("convert display sync position")?;
    if cycle == ProgramCycle::Loop {
        let total = items.iter().try_fold(0_u64, |total, item| {
            let duration = item
                .duration_ms
                .ok_or_else(|| anyhow!("looping display sync item is open-ended"))?;
            total
                .checked_add(u64::from(duration))
                .ok_or_else(|| anyhow!("display sync loop duration overflowed"))
        })?;
        if total == 0 {
            return Err(anyhow!("display sync loop is empty"));
        }
        position = position
            .checked_rem(total)
            .ok_or_else(|| anyhow!("display sync loop duration is zero"))?;
    }

    for (index, item) in items.iter().enumerate() {
        let Some(duration) = item.duration_ms else {
            return playback_at(index, position, None, cycle, alignment);
        };
        if position < u64::from(duration) {
            let remaining = u64::from(duration).saturating_sub(position).max(1);
            let remaining = u32::try_from(remaining).context("convert display sync boundary")?;
            return playback_at(index, position, Some(remaining), cycle, alignment);
        }
        position = position.saturating_sub(u64::from(duration));
    }

    let index = items
        .len()
        .checked_sub(1)
        .ok_or_else(|| anyhow!("display sync program is empty"))?;
    let duration = items
        .get(index)
        .and_then(|item| item.duration_ms)
        .ok_or_else(|| anyhow!("display sync terminal item is invalid"))?;
    playback_at(
        index,
        u64::from(duration.saturating_sub(1)),
        None,
        cycle,
        alignment,
    )
}

fn playback_at(
    index: usize,
    elapsed_ms: u64,
    next_boundary_ms: Option<u32>,
    cycle: ProgramCycle,
    alignment: &PlaybackAlignment,
) -> Result<(DisplayPlayback, Option<u32>)> {
    let current_index = u16::try_from(index).context("convert display sync item index")?;
    let elapsed_ms = u32::try_from(elapsed_ms.min(u64::from(u32::MAX)))
        .context("convert display sync elapsed time")?;
    let refresh = match alignment.mode {
        DisplaySyncMode::StayInSync => next_boundary_ms,
        DisplaySyncMode::Positional => Some(next_boundary_ms.unwrap_or(1_000).min(1_000)),
    };
    Ok((
        DisplayPlayback {
            current_index,
            elapsed_ms,
            cycle,
            sync: Some(DisplaySyncTarget {
                group: alignment.group.clone(),
                mode: alignment.mode,
                sampled_at_unix_ms: alignment.sampled_at_unix_ms,
            }),
        },
        refresh,
    ))
}

fn cycle(cycle: world_interface::display::ProgramCycle) -> ProgramCycle {
    match cycle {
        world_interface::display::ProgramCycle::HoldLast => ProgramCycle::HoldLast,
        world_interface::display::ProgramCycle::Loop => ProgramCycle::Loop,
        world_interface::display::ProgramCycle::PollAtEnd => ProgramCycle::PollAtEnd,
        world_interface::display::ProgramCycle::BlankAtEnd => ProgramCycle::BlankAtEnd,
    }
}

fn source_state(assessment: &DisplayAssessment) -> SourceState {
    match assessment {
        DisplayAssessment::Current => SourceState::Current,
        DisplayAssessment::Unavailable => SourceState::Unavailable,
        DisplayAssessment::Partial(reasons) => SourceState::Partial {
            reasons: reasons.iter().copied().map(partial_reason).collect(),
        },
    }
}

fn partial_reason(reason: DisplayPartialReason) -> display_protocol::program::DisplayPartialReason {
    match reason {
        DisplayPartialReason::ProvisionalData => {
            display_protocol::program::DisplayPartialReason::ProvisionalData
        }
        DisplayPartialReason::CorruptRecords => {
            display_protocol::program::DisplayPartialReason::CorruptRecords
        }
        DisplayPartialReason::IncompleteProjection => {
            display_protocol::program::DisplayPartialReason::IncompleteProjection
        }
        DisplayPartialReason::DegradedSource => {
            display_protocol::program::DisplayPartialReason::DegradedSource
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use display_protocol::program::StaleAction;
    use world_interface::display::{
        DisplayProjection, DisplayResourceId, FrameMediaType, MediaProtocol as SourceMediaProtocol,
        RenderedFrame, RenderedMedia, RenderedProgram, RenderedProgramItem,
    };

    /// The stream program is the one thing a receiver holds for the life of
    /// an assignment, so nothing a program edit changes may reach its
    /// revision. A different refresh hint (a live card ticking, a schedule
    /// boundary moving) compiles to the same bytes on the wire, and the item
    /// is open-ended under a program that holds it — the only shape the
    /// protocol allows without a duration, and the only honest one for a
    /// stream that never ends.
    #[test]
    fn a_program_stream_revision_is_invariant_under_program_edits() {
        let compiler = ProgramCompiler::new([5; 32]).unwrap();
        let assignment = DisplayAssignmentId::parse("aa".repeat(16)).unwrap();
        let program = DisplayProgramId::parse("bb".repeat(16)).unwrap();
        let freshness = FreshnessPolicy {
            stale_after_ms: 60_000,
            on_stale: StaleAction::KeepWithNativeBanner,
        };
        let first = compiler
            .compile_stream(
                &assignment,
                &program,
                freshness.clone(),
                "prog-aa",
                Some(1_000),
            )
            .unwrap();
        let second = compiler
            .compile_stream(&assignment, &program, freshness, "prog-aa", None)
            .unwrap();
        assert_eq!(first.program.revision, second.program.revision);
        assert_eq!(first.program.items.len(), 1);
        let item = &first.program.items[0];
        assert_eq!(item.duration_ms, None, "a stream has no length on the wire");
        assert_eq!(first.program.playback.cycle, ProgramCycle::HoldLast);
        assert!(matches!(
            item.scene,
            DisplayScene::Media {
                protocol: display_protocol::program::MediaProtocol::Hls,
                live: false,
                ..
            }
        ));
        // The hint still reaches the poll's scheduling, off the wire.
        assert_eq!(first.refresh_after_ms, Some(1_000));
        assert_eq!(second.refresh_after_ms, None);
    }

    #[test]
    fn compilation_binds_assets_and_revision_to_the_assignment() {
        let projection = DisplayProjection {
            program: RenderedProgram {
                items: vec![RenderedProgramItem {
                    id: "welcome".into(),
                    duration_ms: None,
                    scene: RenderedScene::Frame(RenderedFrame {
                        media_type: FrameMediaType::Png,
                        width: 1,
                        height: 1,
                        bytes: b"png bytes".to_vec(),
                    }),
                    assessment: DisplayAssessment::Current,
                    spoken_summary: Some("Welcome".into()),
                }],
                cycle: world_interface::display::ProgramCycle::HoldLast,
                refresh_after_ms: Some(500),
            },
            assessment: DisplayAssessment::Current,
            spoken_summary: Some("Lobby".into()),
        };
        let compiler = ProgramCompiler::new([7; 32]).unwrap();
        let compiled = compiler
            .compile(
                &DisplayAssignmentId::parse("11".repeat(16)).unwrap(),
                &DisplayProgramId::parse("22".repeat(16)).unwrap(),
                FreshnessPolicy {
                    stale_after_ms: 60_000,
                    on_stale: StaleAction::Blank,
                },
                projection.clone(),
                None,
                PlaybackTier::Frame,
            )
            .unwrap();
        assert_eq!(compiled.asset_count(), 1);
        assert_eq!(compiled.source_refresh_after_ms, Some(500));
        assert_eq!(compiled.refresh_after_ms, Some(500));
        validate_program(&compiled.program).unwrap();

        let alignment = PlaybackAlignment {
            group: "lobby".into(),
            mode: DisplaySyncMode::Positional,
            epoch_unix_ms: 1_000,
            sampled_at_unix_ms: 2_000,
            static_delay_ms: 250,
        };
        let first = compiler
            .compile(
                &DisplayAssignmentId::parse("33".repeat(16)).unwrap(),
                &DisplayProgramId::parse("44".repeat(16)).unwrap(),
                FreshnessPolicy {
                    stale_after_ms: 60_000,
                    on_stale: StaleAction::Blank,
                },
                projection.clone(),
                Some(&alignment),
                PlaybackTier::Frame,
            )
            .unwrap();
        let second = compiler
            .compile(
                &DisplayAssignmentId::parse("55".repeat(16)).unwrap(),
                &DisplayProgramId::parse("66".repeat(16)).unwrap(),
                FreshnessPolicy {
                    stale_after_ms: 60_000,
                    on_stale: StaleAction::Blank,
                },
                projection,
                Some(&alignment),
                PlaybackTier::Frame,
            )
            .unwrap();
        assert_eq!(first.program.playback.current_index, 0);
        assert_eq!(first.program.playback.elapsed_ms, 1_250);
        assert_eq!(first.program.playback, second.program.playback);
        assert_eq!(
            first.program.playback.sync.as_ref().map(|sync| sync.mode),
            Some(DisplaySyncMode::Positional)
        );
    }

    #[test]
    fn live_media_is_compiled_for_each_receiver_playback_tier() {
        let projection = DisplayProjection {
            program: RenderedProgram {
                items: vec![RenderedProgramItem {
                    id: "live-main".into(),
                    duration_ms: None,
                    scene: RenderedScene::Media(RenderedMedia {
                        origin: world_interface::display::MediaOrigin::Live(
                            DisplayResourceId::new("main").unwrap(),
                        ),
                        protocol: SourceMediaProtocol::Hls,
                    }),
                    assessment: DisplayAssessment::Current,
                    spoken_summary: Some("Live main display".into()),
                }],
                cycle: world_interface::display::ProgramCycle::HoldLast,
                refresh_after_ms: None,
            },
            assessment: DisplayAssessment::Current,
            spoken_summary: None,
        };
        let compiler = ProgramCompiler::new([9; 32]).unwrap();
        let assignment = DisplayAssignmentId::parse("77".repeat(16)).unwrap();
        let program = DisplayProgramId::parse("88".repeat(16)).unwrap();
        let freshness = FreshnessPolicy {
            stale_after_ms: 60_000,
            on_stale: StaleAction::Blank,
        };

        let mse = compiler
            .compile(
                &assignment,
                &program,
                freshness.clone(),
                projection.clone(),
                None,
                PlaybackTier::MseLive,
            )
            .unwrap();
        let hls = compiler
            .compile(
                &assignment,
                &program,
                freshness.clone(),
                projection.clone(),
                None,
                PlaybackTier::NativeHls,
            )
            .unwrap();
        let frame = compiler
            .compile(
                &assignment,
                &program,
                freshness,
                projection,
                None,
                PlaybackTier::Frame,
            )
            .unwrap();

        assert!(matches!(
            &mse.program.items[0].scene,
            DisplayScene::Media { manifest, protocol: display_protocol::program::MediaProtocol::Mse, live: true }
                if manifest.media_type == DisplayAssetMediaType::MseManifest
                    && mse.media_resource(&manifest.id) == Some("main")
        ));
        assert!(matches!(
            &hls.program.items[0].scene,
            DisplayScene::Media { manifest, protocol: display_protocol::program::MediaProtocol::Hls, live: true }
                if manifest.media_type == DisplayAssetMediaType::HlsManifest
                    && hls.media_resource(&manifest.id) == Some("main")
        ));
        assert!(matches!(
            frame.program.items[0].scene,
            DisplayScene::Blank {
                reason: BlankReason::Unsupported
            }
        ));
    }

    fn stored_projection(id: [u8; 32]) -> DisplayProjection {
        DisplayProjection {
            program: RenderedProgram {
                items: vec![RenderedProgramItem {
                    id: "library-film".into(),
                    duration_ms: None,
                    scene: RenderedScene::Media(RenderedMedia {
                        origin: world_interface::display::MediaOrigin::Stored(
                            replica::content::ContentRef { content_id: id },
                        ),
                        protocol: SourceMediaProtocol::Hls,
                    }),
                    assessment: DisplayAssessment::Current,
                    spoken_summary: Some("Ribbon cutting".into()),
                }],
                cycle: world_interface::display::ProgramCycle::HoldLast,
                refresh_after_ms: None,
            },
            assessment: DisplayAssessment::Current,
            spoken_summary: None,
        }
    }

    /// A stored scene is gated by the receiver's tier, exactly as a live one is.
    ///
    /// It used to be gated by neither: the tier was consulted only on the live
    /// branch, so a Frame-tier receiver — which the reference receiver is — was
    /// handed media it cannot draw and was never told it could not draw it. Not
    /// a refusal, not a blank; a screen holding its previous item while health
    /// reported the revision delivered.
    #[test]
    fn stored_media_degrades_on_a_frame_tier_receiver() {
        let compiler = ProgramCompiler::new([9; 32]).unwrap();
        let assignment = DisplayAssignmentId::parse("77".repeat(16)).unwrap();
        let program = DisplayProgramId::parse("88".repeat(16)).unwrap();
        let freshness = FreshnessPolicy {
            stale_after_ms: 60_000,
            on_stale: StaleAction::Blank,
        };

        let hls = compiler
            .compile(
                &assignment,
                &program,
                freshness.clone(),
                stored_projection([0xAB; 32]),
                None,
                PlaybackTier::NativeHls,
            )
            .unwrap();
        let frame = compiler
            .compile(
                &assignment,
                &program,
                freshness,
                stored_projection([0xAB; 32]),
                None,
                PlaybackTier::Frame,
            )
            .unwrap();

        assert!(matches!(
            &hls.program.items[0].scene,
            DisplayScene::Media { manifest, protocol: display_protocol::program::MediaProtocol::Hls, live: false }
                if manifest.media_type == DisplayAssetMediaType::HlsManifest
        ));
        assert!(
            matches!(
                frame.program.items[0].scene,
                DisplayScene::Blank {
                    reason: BlankReason::Unsupported
                }
            ),
            "a receiver that cannot play it is told so"
        );
    }

    /// The two namespaces no longer collide into one asset id.
    ///
    /// `derive_asset_id` commits the media type, length and digest — and the
    /// digest is over the manifest, which now carries the origin. Without that,
    /// a rendition named as 64 hex characters and the content of the same name
    /// derived the same id, and the second one to compile would have been
    /// served the first one's bytes.
    #[test]
    fn a_stored_and_a_live_scene_of_the_same_name_are_different_assets() {
        let compiler = ProgramCompiler::new([9; 32]).unwrap();
        let assignment = DisplayAssignmentId::parse("77".repeat(16)).unwrap();
        let program = DisplayProgramId::parse("88".repeat(16)).unwrap();
        let freshness = FreshnessPolicy {
            stale_after_ms: 60_000,
            on_stale: StaleAction::Blank,
        };
        let name = "ab".repeat(32);

        let stored = compiler
            .compile(
                &assignment,
                &program,
                freshness.clone(),
                stored_projection([0xAB; 32]),
                None,
                PlaybackTier::NativeHls,
            )
            .unwrap();
        let live_projection = DisplayProjection {
            program: RenderedProgram {
                items: vec![RenderedProgramItem {
                    id: "library-film".into(),
                    duration_ms: None,
                    scene: RenderedScene::Media(RenderedMedia {
                        origin: world_interface::display::MediaOrigin::Live(
                            DisplayResourceId::new(name.clone()).unwrap(),
                        ),
                        protocol: SourceMediaProtocol::Hls,
                    }),
                    assessment: DisplayAssessment::Current,
                    spoken_summary: Some("Ribbon cutting".into()),
                }],
                cycle: world_interface::display::ProgramCycle::HoldLast,
                refresh_after_ms: None,
            },
            assessment: DisplayAssessment::Current,
            spoken_summary: None,
        };
        let live = compiler
            .compile(
                &assignment,
                &program,
                freshness,
                live_projection,
                None,
                PlaybackTier::NativeHls,
            )
            .unwrap();

        let (
            DisplayScene::Media {
                manifest: stored_manifest,
                ..
            },
            DisplayScene::Media {
                manifest: live_manifest,
                ..
            },
        ) = (&stored.program.items[0].scene, &live.program.items[0].scene)
        else {
            panic!("both compile to media");
        };
        assert_eq!(
            stored.media_resource(&stored_manifest.id),
            Some(name.as_str()),
            "the stored resource is the content id in hex"
        );
        assert_ne!(
            stored_manifest.id, live_manifest.id,
            "one name, two planes, two assets"
        );
    }
}
