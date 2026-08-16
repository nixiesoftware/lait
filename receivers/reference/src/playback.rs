use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use display_protocol::ids::{DisplayAssetId, DisplayProgramItemId};
use display_protocol::program::{
    BlankReason, DisplayAsset, DisplayPlayback, DisplayProgram, DisplayScene, DisplaySyncTarget,
    ProgramCycle, StaleAction,
};
use display_protocol::receiver::{DisplayedAsset, PlaybackState};
use serde::Serialize;

#[derive(Clone)]
pub struct StagedAsset {
    pub descriptor: DisplayAsset,
    pub path: PathBuf,
}

pub struct Runtime {
    program: DisplayProgram,
    staged: BTreeMap<DisplayAssetId, StagedAsset>,
    current_index: usize,
    elapsed_base_ms: u64,
    item_started_at: Instant,
    delivered_at: Instant,
    last_health_at: Option<Instant>,
    last_sync_residual_ms: i32,
    correction_events: u32,
    ended: bool,
}

impl Runtime {
    pub fn new(program: DisplayProgram, staged: BTreeMap<DisplayAssetId, StagedAsset>) -> Self {
        let current_index = usize::from(program.playback.current_index);
        let elapsed_base_ms = u64::from(program.playback.elapsed_ms);
        let now = Instant::now();
        Self {
            program,
            staged,
            current_index,
            elapsed_base_ms,
            item_started_at: now,
            delivered_at: now,
            // Activation is itself a health transition. Report it immediately
            // so the controller can observe a newly staged program without a
            // thirty-second blind window.
            last_health_at: None,
            last_sync_residual_ms: 0,
            correction_events: 0,
            ended: false,
        }
    }

    pub fn program(&self) -> &DisplayProgram {
        &self.program
    }

    pub fn staged(&self) -> &BTreeMap<DisplayAssetId, StagedAsset> {
        &self.staged
    }

    pub fn mark_delivered(&mut self) {
        self.delivered_at = Instant::now();
    }

    pub fn reconcile(&mut self, response: &DisplayPlayback, sent: &DisplayPlayback) -> Result<()> {
        let mut candidate = self.program.clone();
        candidate.playback = response.clone();
        display_protocol::program::validate_program(&candidate)
            .context("validate no-change playback cursor")?;
        self.mark_delivered();
        let cursor_changed =
            response.current_index != sent.current_index || response.elapsed_ms != sent.elapsed_ms;
        if response.sync.is_some() {
            let residual = if response.current_index == sent.current_index {
                i64::from(response.elapsed_ms)
                    .checked_sub(i64::from(sent.elapsed_ms))
                    .context("calculate display sync residual")?
            } else {
                0
            };
            self.last_sync_residual_ms = i32::try_from(residual.clamp(-60_000, 60_000))
                .context("convert display sync residual")?;
            if cursor_changed {
                self.correction_events = self.correction_events.saturating_add(1);
            }
        } else {
            self.last_sync_residual_ms = 0;
        }
        self.program.playback.sync.clone_from(&response.sync);
        if cursor_changed {
            self.current_index = usize::from(response.current_index);
            self.elapsed_base_ms = u64::from(response.elapsed_ms);
            self.item_started_at = Instant::now();
            self.ended = false;
        }
        Ok(())
    }

    pub fn playback(&mut self) -> Result<DisplayPlayback> {
        self.advance_clock()?;
        let current_index =
            u16::try_from(self.current_index).context("convert receiver playback item index")?;
        let elapsed_ms = u32::try_from(self.elapsed_base_ms.min(u64::from(u32::MAX)))
            .context("convert receiver playback elapsed time")?;
        Ok(DisplayPlayback {
            current_index,
            elapsed_ms,
            cycle: self.program.playback.cycle,
            sync: self.program.playback.sync.clone(),
        })
    }

    pub fn wait_ms(&mut self) -> Result<u32> {
        let playback = self.playback()?;
        let boundary = self
            .program
            .items
            .get(usize::from(playback.current_index))
            .and_then(|item| item.duration_ms)
            .map_or(
                display_protocol::bounds::MAX_LONG_POLL_WAIT_MS,
                |duration| duration.saturating_sub(playback.elapsed_ms).max(1),
            );
        let stale = remaining_ms(self.delivered_at, self.program.freshness.stale_after_ms);
        let health = self
            .last_health_at
            .map_or(1, |reported| remaining_ms(reported, 30_000));
        Ok(boundary
            .min(stale)
            .min(health)
            .clamp(1, display_protocol::bounds::MAX_LONG_POLL_WAIT_MS))
    }

    pub fn health_due(&self) -> bool {
        self.last_health_at
            .is_none_or(|reported| reported.elapsed().as_millis() >= 30_000)
    }

    pub fn mark_health_reported(&mut self) {
        self.last_health_at = Some(Instant::now());
    }

    /// Prioritise a fresh operational sample after any failed live-loop
    /// exchange. The report can only succeed once transport is back, so an
    /// accepted sample is also evidence that reauthentication recovered.
    pub fn mark_health_due(&mut self) {
        self.last_health_at = None;
    }

    pub const fn last_sync_residual_ms(&self) -> i32 {
        self.last_sync_residual_ms
    }

    pub const fn correction_events(&self) -> u32 {
        self.correction_events
    }

    pub fn should_refresh_snapshot(&self) -> bool {
        self.ended && self.program.playback.cycle == ProgramCycle::PollAtEnd
    }

    pub fn view(&mut self) -> Result<PlaybackView<'_>> {
        let playback = self.playback()?;
        let stale = self.delivered_at.elapsed().as_millis()
            >= u128::from(self.program.freshness.stale_after_ms);
        let item = self
            .program
            .items
            .get(usize::from(playback.current_index))
            .ok_or_else(|| anyhow!("receiver playback item is absent"))?;
        let scene = if self.ended && self.program.playback.cycle != ProgramCycle::HoldLast {
            PlaybackViewScene::Blank(BlankReason::ProgramEnded)
        } else if stale && self.program.freshness.on_stale == StaleAction::Blank {
            PlaybackViewScene::Blank(BlankReason::HostUnavailable)
        } else {
            match &item.scene {
                DisplayScene::Frame { asset } => {
                    let staged = self
                        .staged
                        .get(&asset.id)
                        .ok_or_else(|| anyhow!("current frame was not staged"))?;
                    PlaybackViewScene::Frame(staged)
                }
                DisplayScene::Blank { reason } => PlaybackViewScene::Blank(*reason),
                DisplayScene::Media { .. } => PlaybackViewScene::Unsupported,
            }
        };
        Ok(PlaybackView {
            program: &self.program,
            item: &item.id,
            playback,
            stale,
            scene,
        })
    }

    pub fn health_sample(
        &mut self,
    ) -> Result<(
        DisplayPlayback,
        DisplayProgramItemId,
        Option<DisplayedAsset>,
        PlaybackState,
        u16,
        u32,
    )> {
        let staged_items =
            u16::try_from(self.staged.len()).context("convert staged receiver item count")?;
        let staged_bytes = self.staged.values().try_fold(0_u32, |total, staged| {
            total
                .checked_add(staged.descriptor.encoded_len)
                .ok_or_else(|| anyhow!("staged receiver byte count overflow"))
        })?;
        let view = self.view()?;
        let (displayed, playback_state) = match view.scene {
            PlaybackViewScene::Frame(staged) => (
                Some(DisplayedAsset {
                    id: staged.descriptor.id.clone(),
                    sha256: staged.descriptor.sha256.clone(),
                }),
                PlaybackState::Displaying,
            ),
            PlaybackViewScene::Blank(_) => (None, PlaybackState::Blank),
            PlaybackViewScene::Unsupported => (None, PlaybackState::Unsupported),
        };
        Ok((
            view.playback,
            view.item.clone(),
            displayed,
            playback_state,
            staged_items,
            staged_bytes,
        ))
    }

    fn advance_clock(&mut self) -> Result<()> {
        if self.ended {
            return Ok(());
        }
        let elapsed = u64::try_from(self.item_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut position = self.elapsed_base_ms.saturating_add(elapsed);
        loop {
            let item = self
                .program
                .items
                .get(self.current_index)
                .ok_or_else(|| anyhow!("receiver playback item is absent"))?;
            let Some(duration) = item.duration_ms else {
                break;
            };
            if position < u64::from(duration) {
                break;
            }
            position = position.saturating_sub(u64::from(duration));
            let next = self.current_index.saturating_add(1);
            if next < self.program.items.len() {
                self.current_index = next;
                continue;
            }
            match self.program.playback.cycle {
                ProgramCycle::Loop => self.current_index = 0,
                ProgramCycle::HoldLast => {
                    position = u64::from(duration.saturating_sub(1));
                    self.ended = true;
                    break;
                }
                ProgramCycle::PollAtEnd | ProgramCycle::BlankAtEnd => {
                    position = u64::from(duration.saturating_sub(1));
                    self.ended = true;
                    break;
                }
            }
        }
        self.elapsed_base_ms = position;
        self.item_started_at = Instant::now();
        Ok(())
    }
}

fn remaining_ms(since: Instant, interval_ms: u32) -> u32 {
    let elapsed = since.elapsed().as_millis();
    let remaining = u128::from(interval_ms).saturating_sub(elapsed).max(1);
    u32::try_from(remaining).unwrap_or(u32::MAX)
}

pub enum PlaybackViewScene<'a> {
    Frame(&'a StagedAsset),
    Blank(BlankReason),
    Unsupported,
}

pub struct PlaybackView<'a> {
    program: &'a DisplayProgram,
    item: &'a DisplayProgramItemId,
    playback: DisplayPlayback,
    stale: bool,
    scene: PlaybackViewScene<'a>,
}

pub struct Presenter {
    output: PathBuf,
    last_presentation: Option<String>,
}

impl Presenter {
    pub fn open(output: PathBuf) -> Result<Self> {
        fs::create_dir_all(&output).context("create receiver presentation directory")?;
        Ok(Self {
            output,
            last_presentation: None,
        })
    }

    pub fn present(&mut self, view: &PlaybackView<'_>) -> Result<()> {
        let scene_key = match view.scene {
            PlaybackViewScene::Frame(staged) => staged.descriptor.id.as_str(),
            PlaybackViewScene::Blank(_) => "blank",
            PlaybackViewScene::Unsupported => "unsupported",
        };
        let key = format!(
            "{}:{}:{}:{scene_key}",
            view.program.revision, view.item, view.stale
        );
        if self.last_presentation.as_deref() == Some(&key) {
            return Ok(());
        }
        let scene = match view.scene {
            PlaybackViewScene::Frame(staged) => {
                atomic_copy(&staged.path, &self.output.join("frame.png"))?;
                PresentedScene::Frame { path: "frame.png" }
            }
            PlaybackViewScene::Blank(reason) => PresentedScene::Blank { reason },
            PlaybackViewScene::Unsupported => PresentedScene::Unsupported,
        };
        let status = PresentedStatus {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            assignment: view.program.assignment.as_str(),
            program: view.program.program.as_str(),
            revision: view.program.revision.as_str(),
            item: view.item.as_str(),
            elapsed_ms: view.playback.elapsed_ms,
            sync: view.playback.sync.as_ref(),
            stale: view.stale,
            scene,
        };
        atomic_json(&self.output.join("active.json"), &status)?;
        self.last_presentation = Some(key);
        Ok(())
    }

    pub fn unassigned(&mut self, device: &str) -> Result<()> {
        let status = serde_json::json!({
            "protocol_major": display_protocol::PROTOCOL_MAJOR,
            "device": device,
            "scene": { "kind": "blank", "reason": "unassigned" }
        });
        atomic_json(&self.output.join("active.json"), &status)?;
        self.last_presentation = Some("unassigned".to_owned());
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PresentedScene<'a> {
    Frame { path: &'a str },
    Blank { reason: BlankReason },
    Unsupported,
}

#[derive(Serialize)]
struct PresentedStatus<'a> {
    protocol_major: u32,
    assignment: &'a str,
    program: &'a str,
    revision: &'a str,
    item: &'a str,
    elapsed_ms: u32,
    sync: Option<&'a DisplaySyncTarget>,
    stale: bool,
    scene: PresentedScene<'a>,
}

fn atomic_copy(source: &Path, target: &Path) -> Result<()> {
    let temporary = target.with_extension("png.tmp");
    fs::copy(source, &temporary).context("copy staged frame to presentation candidate")?;
    OpenOptions::new()
        .write(true)
        .open(&temporary)
        .context("open presentation candidate for flush")?
        .sync_all()
        .context("flush presentation candidate")?;
    mechanics::secretfs::persist_replace(&temporary, target)
        .context("atomically present receiver frame")
}

fn atomic_json<T: Serialize>(target: &Path, value: &T) -> Result<()> {
    let temporary = target.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).context("encode receiver presentation status")?;
    let mut file = File::create(&temporary).context("create presentation status candidate")?;
    file.write_all(&bytes)
        .context("write presentation status candidate")?;
    file.sync_all()
        .context("flush presentation status candidate")?;
    drop(file);
    mechanics::secretfs::persist_replace(&temporary, target)
        .context("atomically present receiver status")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_frame_is_flushed_and_presented_atomically() {
        let root =
            std::env::temp_dir().join(format!("astrolabe-reference-frame-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create frame test root");
        let staged = root.join("staged.png");
        let presented = root.join("frame.png");
        fs::write(&staged, b"verified-frame").expect("write staged frame");

        atomic_copy(&staged, &presented).expect("present first frame");

        assert_eq!(fs::read(&presented).unwrap(), b"verified-frame");
        assert!(!presented.with_extension("png.tmp").exists());
        let _ = fs::remove_dir_all(root);
    }
}
