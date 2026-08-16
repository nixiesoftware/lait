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
use world_interface::display::{
    DisplayAssessment, DisplayPartialReason, DisplayProjection, FrameMediaType, RenderedScene,
};

pub struct CompiledProgram {
    pub program: DisplayProgram,
    /// Package-selected boundary after which the same Query must be projected
    /// again even if the underlying World has emitted no invalidation.
    pub refresh_after_ms: Option<u32>,
    assets: BTreeMap<DisplayAssetId, Vec<u8>>,
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
    ) -> Result<CompiledProgram> {
        let mut refresh_after_ms = projection.program.refresh_after_ms;
        let mut assets = BTreeMap::new();
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
                RenderedScene::Media(_) => {
                    return Err(anyhow!(
                        "package returned media before a display resource provider was registered"
                    ));
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
            refresh_after_ms,
            assets,
        })
    }
}

fn aligned_playback(
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
        DisplayProjection, FrameMediaType, RenderedFrame, RenderedProgram, RenderedProgramItem,
    };

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
            )
            .unwrap();
        assert_eq!(compiled.asset_count(), 1);
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
}
