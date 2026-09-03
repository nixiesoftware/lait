//! Durable receiver, assignment, and health coordination.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use display_protocol::ids::{
    DisplayAssetId, DisplayAssignmentId, DisplayDeviceId, DisplayProgramId, DisplayProgramItemId,
    ProgramRevision,
};
use display_protocol::program::{DisplayProgram, DisplayScene, DisplaySyncMode};
use display_protocol::receiver::{
    validate_capabilities, PlaybackTier, ReceiverCapabilities, SyncClass,
};
use replica::body::WorldId;
use runtime::world::call::{Call, Reply};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, Notify};
use world_interface::display::{
    BlankReason, DisplayChoice, DisplayProjection, DisplayRequest, DisplaySurfaceId,
    FrameMediaType, RenderedFrame, RenderedScene, StoredFrame, MAX_RENDERED_ASSET_BYTES,
    REQUIRED_WORLD_ACCESS,
};
use world_interface::{
    ClientAccess, ClientFuture, ClientHost, ClientInvocationKind, Failure, HostContentRequest,
    HostControlRequest, PresentationHandle, PresentationResolution, WorldClientRegistry,
};

use crate::control::{ControlRoute, Request};
use crate::orbits::Router;

use super::producer::{self, ClipReader, ReadFuture, Splice, StillCache, Timeline};
use super::{
    AssignmentRecord, CompiledProgram, CoordinatorPolicy, CoordinatorStore, PlaybackAlignment,
    ProgramCompiler,
};
use super::{LiveMediaHub, LiveTransport};

/// One exact surface to render, and the viewport to render it for.
///
/// This is the whole of what a render needs. An attached receiver's assignment
/// is turned into one of these and pinned besides; a member screen builds one
/// directly from a local choice. Keeping the two on one path is what makes the
/// member profile a *reach and revocation* difference rather than a second
/// rendering stack that could disagree with the first.
#[derive(Debug, Clone)]
pub struct SurfaceRender {
    pub orbit: String,
    /// The Space an assignment pins. `None` when nothing has committed to one
    /// yet, in which case the resolved Orbit is taken as the answer rather than
    /// checked against a second one.
    pub space: Option<String>,
    pub world: WorldId,
    pub surface: world_interface::display::DisplaySurfaceId,
    pub input: world_interface::display::CanonicalDisplayInput,
    pub theme: world_interface::display::DisplayTheme,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u16,
    pub locale: String,
    pub horizon_ms: u32,
    pub now_unix_ms: u64,
}

const MAX_LIVE_TICKETS: usize = 256;
const MSE_TICKET_LIFETIME_MS: u64 = 86_400_000;
const HLS_TICKET_LIFETIME_MS: u64 = 86_400_000;
const LIVE_SOURCE_WAIT: Duration = Duration::from_secs(2);

/// Product-neutral, self-hosted display coordination.
///
/// The coordinator owns receiver-facing identifiers and frozen output. World
/// packages own input parsing and projection. The daemon remains the authority
/// for Orbit placement, trusted Query classification, and World execution.
pub struct DisplayCoordinator {
    store: Arc<CoordinatorStore>,
    router: Arc<Router>,
    registry: WorldClientRegistry,
    compiler: ProgramCompiler,
    local_root: PathBuf,
    compiled: Mutex<BTreeMap<String, Arc<CompiledProgram>>>,
    /// One producer per assignment a native-HLS receiver is playing: the
    /// schedule behind its stream, edited at the live edge and never rebuilt.
    producers: Mutex<BTreeMap<String, Arc<Producer>>>,
    assignment_changes: broadcast::Sender<()>,
    live: LiveMediaHub,
    live_tickets: Mutex<BTreeMap<String, LiveTicket>>,
    /// What each assignment's stream must remember across a restart, by
    /// assignment id: the epoch it counts from, and the discontinuities its
    /// window has dropped. Persisted beside the tickets so a restarted daemon
    /// continues the same numbering in both playlist headers, and a receiver
    /// that kept its URL never sees either go backwards.
    producer_epochs: Mutex<BTreeMap<String, ProducerMemory>>,
    /// When each device's compiled program last changed revision. A ticket
    /// minted for the revision before is honoured for a grace after that,
    /// so a receiver re-staging for a new stream is never refused the old
    /// one mid-reload — that refusal was the "decode failed" a person saw at
    /// every deliberate re-stage.
    revision_changes: Mutex<BTreeMap<String, u64>>,
}

/// How long a ticket outlives the revision it was minted for.
const RESTAGE_GRACE_MS: u64 = 15_000;

/// Whether a ticket whose program was revised is still to be honoured:
/// within the grace after the change, yes.
const fn within_restage_grace(changed_at_unix_ms: Option<u64>, now_unix_ms: u64) -> bool {
    match changed_at_unix_ms {
        Some(changed) => now_unix_ms.saturating_sub(changed) <= RESTAGE_GRACE_MS,
        None => false,
    }
}

/// What a stream remembers across restarts.
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct ProducerMemory {
    epoch_unix_ms: u64,
    #[serde(default)]
    discontinuities_dropped: u64,
    /// The window's first sequence when `discontinuities_dropped` was last
    /// recorded: the count is the discontinuities before *this* sequence.
    /// A restart resumes a trail behind the edge, past sequences nobody's
    /// window dropped, and counts the seams the schedule carries from here
    /// to there — without the anchor a playlist came out one short after a
    /// restart, and a strict player answered with a 12 s re-sync.
    #[serde(default)]
    window_first_sequence: Option<u64>,
}

/// The dropped-discontinuity count a resumed window declares: the count
/// recorded before `anchor`, moved to `first` by the seams the schedule
/// carries between the two. Without an anchor the recorded count stands.
fn resumed_discontinuities(
    recorded: u64,
    anchor: Option<u64>,
    first: u64,
    before: impl Fn(u64) -> u64,
) -> u64 {
    match anchor {
        Some(anchor) if anchor <= first => {
            recorded.saturating_add(before(first).saturating_sub(before(anchor)))
        }
        Some(anchor) => recorded.saturating_sub(before(anchor).saturating_sub(before(first))),
        None => recorded,
    }
}

/// Where tickets and epochs are kept across restarts: beside the package
/// state, as plain JSON. A ticket is a bearer secret for one stream; the
/// file is owner-readable like everything else under the display root.
const TICKETS_FILE: &str = "live-tickets.json";
const EPOCHS_FILE: &str = "producer-epochs.json";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct LiveTicket {
    device: DisplayDeviceId,
    assignment: DisplayAssignmentId,
    program: DisplayProgramId,
    revision: ProgramRevision,
    current_item: DisplayProgramItemId,
    manifest: DisplayAssetId,
    orbit: String,
    resource: String,
    transport: LiveTransport,
    expires_at_unix_ms: u64,
    /// When a newer ticket for the same item replaced this one. The player
    /// keeps asking on the old ticket while the receiver swaps to the new
    /// stream, so it is honoured through the re-stage grace and refused after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded_at_unix_ms: Option<u64>,
}

#[derive(Clone)]
pub(crate) struct AuthorizedLiveStream {
    pub orbit: String,
    pub resource: String,
    ticket: LiveTicket,
}

pub(crate) struct LiveTicketGrant {
    pub token: String,
    pub expires_at_unix_ms: u64,
}

/// Receivers a long poll arms before its first compile, closing the race where
/// an assignment or World changes between rendering and beginning to wait.
pub struct DisplayChangeSubscriptions {
    assignments: broadcast::Receiver<()>,
    worlds: broadcast::Receiver<crate::orbits::OrbitDoorbell>,
}

/// The content plane as a producer reads it: bytes by content id and range,
/// through the daemon's own channel, fetched from a peer when this Station
/// lacks them.
struct ContentReads {
    home: PathBuf,
    route: ControlRoute,
}

impl ClipReader for ContentReads {
    fn size<'a>(&'a self, resource: &'a str) -> ReadFuture<'a, u64> {
        Box::pin(async move {
            match crate::control::content_call(
                &self.home,
                &crate::control::content_request(
                    self.route.clone(),
                    crate::control::ContentCall::Stat {
                        content: resource.to_string(),
                    },
                ),
            )
            .await
            {
                Ok((crate::control::ContentReply::ContentStatus { plaintext_len, .. }, _)) => {
                    Ok(plaintext_len)
                }
                Ok((reply, _)) => Err(anyhow!("clip stat refused: {reply:?}")),
                Err(error) => Err(error).context("stat clip"),
            }
        })
    }

    fn read<'a>(&'a self, resource: &'a str, offset: u64, len: u64) -> ReadFuture<'a, Vec<u8>> {
        Box::pin(read_stored(&self.home, &self.route, resource, offset, len))
    }
}

/// The stream one receiver holds for the life of its assignment, and the
/// task that keeps it made. See [`super::producer`] for the schedule.
pub(crate) struct Producer {
    assignment: AssignmentRecord,
    capabilities: ReceiverCapabilities,
    orbit_key: String,
    resource: String,
    reads: ContentReads,
    started_at_unix_ms: u64,
    /// The epoch this assignment's stream counted from before this process,
    /// if it ran before; a restart continues it.
    stored_epoch_unix_ms: Option<u64>,
    /// The discontinuities the window had dropped before this process, and
    /// the window's first sequence when that was recorded.
    resumed_discontinuities_dropped: u64,
    resumed_window_first_sequence: Option<u64>,
    state: tokio::sync::Mutex<ProducerState>,
    running: AtomicBool,
    /// Cleared when the program stops being one stream, or the producer is
    /// replaced; the task sees it and leaves.
    active: AtomicBool,
    wake: Notify,
}

struct ProducerState {
    timeline: Option<Timeline>,
    cache: StillCache,
    /// When the producer next renders on its own: the World's refresh
    /// cadence, or never, when the program only changes on a doorbell.
    next_render_unix_ms: u64,
}

impl Producer {
    /// A screen nobody has asked a segment from for this long is off; the
    /// producer stops making segments and keeps its place, so a fetch that
    /// comes back continues the same sequence.
    const IDLE_STOP_MS: u64 = 90_000;
    /// A render that failed keeps the last schedule playing and is retried
    /// after this long.
    const RENDER_RETRY_MS: u64 = 5_000;
    /// The floor on a World's render cadence.
    const RENDER_FLOOR_MS: u64 = 250;
    /// How long the task waits with nothing due, so a stopped screen is
    /// noticed and a lost doorbell costs no more than this.
    const IDLE_CHECK_MS: u64 = 5_000;

    fn serves(&self, assignment: &AssignmentRecord, capabilities: &ReceiverCapabilities) -> bool {
        self.assignment.id == assignment.id
            && self.assignment.program == assignment.program
            && self.assignment.theme == assignment.theme
            && self.assignment.source.input == assignment.source.input
            && self.capabilities.viewport == capabilities.viewport
            && self.capabilities.locale == capabilities.locale
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Whether a lap has ever been laid on this producer's timeline. A
    /// producer is registered before its first feed, and that feed runs
    /// inside the request that asked for it — a receiver that hangs up
    /// mid-way cancels it, and what is left must be fed again, not served.
    async fn has_program(&self) -> bool {
        self.state.lock().await.timeline.is_some()
    }

    fn retire(&self) {
        self.active.store(false, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// The epoch the timeline counts from: the sync group's, so members place
    /// the same edge at the same instant, or this producer's first start.
    fn epoch(&self, now_unix_ms: u64) -> u64 {
        self.assignment.sync.as_ref().map_or(
            self.stored_epoch_unix_ms.unwrap_or(now_unix_ms),
            |sync| {
                sync.epoch_unix_ms
                    .saturating_add_signed(i64::from(sync.static_delay_ms))
            },
        )
    }

    /// The size to render cards at: the stream's frame once a lap has
    /// decided it (a clip's size, when there is one), else the screen's.
    async fn render_size(&self) -> (u32, u32) {
        let state = self.state.lock().await;
        state.timeline.as_ref().map_or(
            (
                self.capabilities.viewport.width,
                self.capabilities.viewport.height,
            ),
            |timeline| timeline.frame(),
        )
    }

    fn surface_render(&self, now_unix_ms: u64, size: (u32, u32)) -> Result<SurfaceRender> {
        let world = WorldId::parse(&self.assignment.source.world)
            .ok_or_else(|| anyhow!("display assignment pins an invalid World"))?;
        Ok(SurfaceRender {
            orbit: self.assignment.orbit.clone(),
            space: Some(self.assignment.space.clone()),
            world,
            surface: self.assignment.source.surface.clone(),
            input: self.assignment.source.input.clone(),
            theme: self.assignment.theme,
            width: size.0,
            height: size.1,
            scale_milli: self.capabilities.viewport.scale_milli,
            locale: self.capabilities.locale.clone(),
            horizon_ms: self.capabilities.max_staging_horizon_ms,
            now_unix_ms,
        })
    }

    /// Take a rendered program: build its lap and lay it on the timeline —
    /// opening the timeline if this is the first — then make the window
    /// through the edge so a receiver has something to play at once.
    ///
    /// The lap is built outside the state lock, because building it decodes
    /// and encodes pictures — seconds, in a debug build — and the keeper
    /// making segments must not wait on that.
    /// `Ok(true)` when the stream's frame changed with this lap: the stream
    /// program's revision now differs and the receiver must be woken to
    /// re-stage for it.
    async fn feed(
        &self,
        live: &LiveMediaHub,
        projection: &DisplayProjection,
        now_unix_ms: u64,
    ) -> Result<bool> {
        let viewport = (
            self.capabilities.viewport.width,
            self.capabilities.viewport.height,
        );
        let mut cache = self.state.lock().await.cache.clone();
        let started = std::time::Instant::now();
        let lap = producer::build_lap(projection, &mut cache, &self.reads, viewport).await?;
        tracing::debug!(
            resource = %self.resource,
            elapsed_ms = started.elapsed().as_millis(),
            "program lap built"
        );
        let mut state = self.state.lock().await;
        state.cache = cache;
        let frame_before = state.timeline.as_ref().map(Timeline::frame);
        match state.timeline.as_mut() {
            Some(timeline) => match timeline.offer(lap) {
                Splice::InPlace => {
                    tracing::debug!(resource = %self.resource, "program pictures swapped in place");
                }
                Splice::Era { at } => {
                    tracing::debug!(resource = %self.resource, at, "program spliced at the edge");
                }
            },
            None => {
                let epoch = self.epoch(now_unix_ms);
                state.timeline = Some(Timeline::new(&self.resource, lap, epoch, now_unix_ms));
            }
        }
        state.next_render_unix_ms = projection
            .program
            .refresh_after_ms
            .map_or(u64::MAX, |refresh| {
                now_unix_ms.saturating_add(u64::from(refresh).max(Self::RENDER_FLOOR_MS))
            });
        // Cards rendered at a size other than the stream's frame were fitted
        // onto it; render them again at the frame itself, so a card is drawn
        // for the size it is shown at rather than scaled to it.
        let rendered_at = projection
            .program
            .items
            .iter()
            .find_map(|item| match &item.scene {
                RenderedScene::Frame(picture) => Some((picture.width, picture.height)),
                _ => None,
            });
        if let (Some(timeline), Some(rendered_at)) = (state.timeline.as_ref(), rendered_at) {
            if timeline.frame() != rendered_at {
                state.next_render_unix_ms = 0;
                self.wake.notify_one();
            }
        }
        let frame_now = state.timeline.as_ref().map(Timeline::frame);
        let frame_changed = frame_before.is_some() && frame_now != frame_before;
        if frame_changed {
            tracing::info!(resource = %self.resource, ?frame_before, ?frame_now, "program stream changed size; the receiver will re-stage");
        }
        self.ensure_window_locked(live, &mut state, now_unix_ms)
            .await?;
        Ok(frame_changed)
    }

    /// The frame the stream is coded at, once a lap has decided it.
    async fn frame(&self) -> Option<(u32, u32)> {
        self.state
            .lock()
            .await
            .timeline
            .as_ref()
            .map(Timeline::frame)
    }

    /// Make sure the presentation exists and the window through the edge is
    /// made, for a producer that was idle or is being asked for the first
    /// time since it was fed.
    async fn ensure_window(&self, live: &LiveMediaHub, now_unix_ms: u64) -> Result<()> {
        let mut state = self.state.lock().await;
        self.ensure_window_locked(live, &mut state, now_unix_ms)
            .await
    }

    async fn ensure_window_locked(
        &self,
        live: &LiveMediaHub,
        state: &mut ProducerState,
        now_unix_ms: u64,
    ) -> Result<()> {
        let timeline = state
            .timeline
            .as_mut()
            .ok_or_else(|| anyhow!("producer has no program yet"))?;
        if !live.has_resource(&self.orbit_key, &self.resource, LiveTransport::Hls)
            && live.fetched_at(&self.orbit_key, &self.resource).is_none()
        {
            let description = timeline.description();
            // What the window must declare it has dropped: what the previous
            // process had recorded, moved from the sequence it recorded it
            // before to the first sequence this process makes by the seams
            // the schedule carries between them — the ones a restart skips.
            let dropped = resumed_discontinuities(
                self.resumed_discontinuities_dropped,
                self.resumed_window_first_sequence,
                timeline.next(),
                |n| timeline.discontinuities_before(n),
            );
            live.install_rolling(
                &self.orbit_key,
                &self.resource,
                description,
                producer::WINDOW,
                dropped,
            )?;
            live.touch(&self.orbit_key, &self.resource, now_unix_ms);
        }
        timeline.catch_up(now_unix_ms);
        let target = timeline.target_through(now_unix_ms);
        while timeline.next() <= target {
            let started = std::time::Instant::now();
            let segment = timeline.materialise_next(&self.reads).await?;
            tracing::debug!(
                resource = %self.resource,
                sequence = segment.group_sequence,
                discontinuity = segment.discontinuity,
                bytes = segment.bytes.len(),
                elapsed_ms = started.elapsed().as_millis(),
                "program segment made"
            );
            // A desk aid: with `LAIT_DISPLAY_DUMP_DIR` set, every segment made
            // is written where ffprobe can read it.
            if let Ok(dir) = std::env::var("LAIT_DISPLAY_DUMP_DIR") {
                let path = std::path::Path::new(&dir)
                    .join(format!("{}-{}.ts", self.resource, segment.group_sequence));
                if let Err(error) = std::fs::write(&path, &segment.bytes) {
                    tracing::debug!(error = %error, "segment dump failed");
                }
            }
            live.push_hls_segment(&self.orbit_key, &self.resource, segment)?;
        }
        Ok(())
    }
}

impl DisplayCoordinator {
    /// Remember a stream's dropped-discontinuity count when it changed, so a
    /// restart resumes the playlist header where it was.
    fn remember_discontinuities(&self, producer: &Producer) {
        let Some(dropped) = self
            .live
            .discontinuities_dropped(&producer.orbit_key, &producer.resource)
        else {
            return;
        };
        let first = self
            .live
            .window_first_sequence(&producer.orbit_key, &producer.resource);
        let changed = self
            .producer_epochs
            .lock()
            .ok()
            .and_then(|mut epochs| {
                let memory = epochs.get_mut(producer.assignment.id.as_str())?;
                // Recorded when the count moves: "`dropped` seams before
                // `first`" stays true as later seamless segments drop, so
                // the file is not rewritten once a second for the window's
                // slide.
                (memory.discontinuities_dropped != dropped).then(|| {
                    memory.discontinuities_dropped = dropped;
                    memory.window_first_sequence = first;
                })
            })
            .is_some();
        if changed {
            self.persist_epochs();
        }
    }
}

/// The keeper: keeps the window made ahead of the clock, and stops when the
/// screen does. It never renders — a render is seconds in a debug build and
/// the segment the player is about to ask for cannot wait on it — so the
/// renderer is a task of its own and the two meet at the state lock, briefly.
async fn run_keeper(coordinator: Weak<DisplayCoordinator>, producer: Arc<Producer>) {
    tracing::debug!(resource = %producer.resource, "program producer running");
    loop {
        let Some(coordinator) = coordinator.upgrade() else {
            break;
        };
        if !producer.is_active() {
            break;
        }
        let now = mechanics::wallclock::now_millis();
        let alive = coordinator
            .active_assignment_for_device(&producer.assignment.device, now)
            .ok()
            .flatten()
            .is_some_and(|assignment| assignment.id == producer.assignment.id);
        if !alive {
            coordinator
                .live
                .remove_stored(&producer.orbit_key, &producer.resource);
            coordinator.retire_producer(&producer.assignment.id);
            break;
        }
        let fetched = coordinator
            .live
            .fetched_at(&producer.orbit_key, &producer.resource)
            .unwrap_or(0)
            .max(producer.started_at_unix_ms);
        if now.saturating_sub(fetched) > Producer::IDLE_STOP_MS {
            // Off, or unreachable. The presentation goes; the timeline stays,
            // so the next poll continues the same sequence from the clock.
            coordinator
                .live
                .remove_stored(&producer.orbit_key, &producer.resource);
            tracing::debug!(resource = %producer.resource, "program producer idle; stopping");
            break;
        }
        let segment_due = {
            let mut state = producer.state.lock().await;
            if let Err(error) = producer
                .ensure_window_locked(&coordinator.live, &mut state, now)
                .await
            {
                tracing::warn!(resource = %producer.resource, error = %format_args!("{error:#}"), "program segment could not be made");
            }
            coordinator.remember_discontinuities(&producer);
            state.timeline.as_ref().map_or(
                now.saturating_add(Producer::IDLE_CHECK_MS),
                |timeline| {
                    timeline
                        .start_ms(timeline.next())
                        .saturating_sub(producer::LEAD_MS)
                },
            )
        };
        drop(coordinator);
        let wait = segment_due
            .min(now.saturating_add(Producer::IDLE_CHECK_MS))
            .saturating_sub(now)
            .clamp(50, Producer::IDLE_CHECK_MS);
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(wait)) => {}
            () = producer.wake.notified() => {}
        }
    }
    tracing::debug!(resource = %producer.resource, "program producer stopped");
    producer.running.store(false, Ordering::SeqCst);
    producer.wake.notify_waiters();
}

/// The renderer: renders on the World's cadence and on its doorbell, and
/// feeds what it rendered to the timeline. It leaves when the keeper does.
async fn run_renderer(coordinator: Weak<DisplayCoordinator>, producer: Arc<Producer>) {
    loop {
        let Some(coordinator) = coordinator.upgrade() else {
            break;
        };
        if !producer.is_active() || !producer.running.load(Ordering::SeqCst) {
            break;
        }
        let now = mechanics::wallclock::now_millis();
        let next_render = producer.state.lock().await.next_render_unix_ms;
        if next_render <= now {
            // Render for the slot being made, not for now: the segment this
            // picture lands in begins a lead ahead of the clock.
            let started = std::time::Instant::now();
            let size = producer.render_size().await;
            let rendered =
                match producer.surface_render(now.saturating_add(producer::LEAD_MS), size) {
                    Ok(want) => {
                        coordinator
                            .render_surface(&want, Some(&producer.assignment))
                            .await
                    }
                    Err(error) => Err(error),
                };
            tracing::debug!(
                resource = %producer.resource,
                elapsed_ms = started.elapsed().as_millis(),
                ok = rendered.is_ok(),
                "program rendered"
            );
            match rendered {
                Ok(projection) => {
                    if let Some(reason) = producer::unstreamable(&projection) {
                        // The program stopped being one stream. The receiver
                        // is told through its poll and gets the per-item
                        // program; this producer is done.
                        tracing::info!(resource = %producer.resource, reason, "program left the stream path");
                        coordinator
                            .live
                            .remove_stored(&producer.orbit_key, &producer.resource);
                        coordinator.retire_producer(&producer.assignment.id);
                        coordinator.notify_assignment_change();
                        break;
                    }
                    match producer.feed(&coordinator.live, &projection, now).await {
                        Ok(true) => coordinator.notify_assignment_change(),
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(resource = %producer.resource, error = %format_args!("{error:#}"), "program render could not be scheduled; the last one plays on");
                            producer.state.lock().await.next_render_unix_ms =
                                now.saturating_add(Producer::RENDER_RETRY_MS);
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(resource = %producer.resource, error = %format_args!("{error:#}"), "program render failed; the last one plays on");
                    producer.state.lock().await.next_render_unix_ms =
                        now.saturating_add(Producer::RENDER_RETRY_MS);
                }
            }
            continue;
        }
        let wait = next_render
            .min(now.saturating_add(Producer::IDLE_CHECK_MS))
            .saturating_sub(now)
            .clamp(50, Producer::IDLE_CHECK_MS);
        let subscriptions = coordinator.subscribe_changes();
        tokio::select! {
            changed = coordinator.wait_for_change(
                &producer.assignment,
                subscriptions,
                Duration::from_millis(wait),
            ) => {
                if changed {
                    producer.state.lock().await.next_render_unix_ms = 0;
                }
            }
            () = producer.wake.notified() => {}
        }
    }
}

impl DisplayCoordinator {
    pub fn new(
        store: Arc<CoordinatorStore>,
        router: Arc<Router>,
        registry: WorldClientRegistry,
        local_root: PathBuf,
    ) -> Result<Self> {
        let identifier_key = store.identifier_key()?;
        let tickets: BTreeMap<String, LiveTicket> = load_json(&local_root.join(TICKETS_FILE));
        let epochs: BTreeMap<String, ProducerMemory> = load_json(&local_root.join(EPOCHS_FILE));
        let (assignment_changes, _) = broadcast::channel(64);
        Ok(Self {
            store,
            router,
            registry,
            compiler: ProgramCompiler::new(identifier_key)?,
            local_root,
            compiled: Mutex::new(BTreeMap::new()),
            producers: Mutex::new(BTreeMap::new()),
            assignment_changes,
            live: LiveMediaHub::default(),
            live_tickets: Mutex::new(tickets),
            producer_epochs: Mutex::new(epochs),
            revision_changes: Mutex::new(BTreeMap::new()),
        })
    }

    /// Write the ticket table, so a restart does not invalidate every
    /// receiver's stream URL — the failure that had a screen hammering a
    /// dead ticket eighty times a second until its poll got through.
    fn persist_tickets(&self) {
        if let Ok(tickets) = self.live_tickets.lock() {
            save_json(&self.local_root.join(TICKETS_FILE), &*tickets);
        }
    }

    fn persist_epochs(&self) {
        if let Ok(epochs) = self.producer_epochs.lock() {
            save_json(&self.local_root.join(EPOCHS_FILE), &*epochs);
        }
    }

    /// The device a ticket names, without authorising it — so a request
    /// arriving before this process has compiled that device's program can
    /// compile it first.
    pub(crate) fn ticket_device(&self, token: &str) -> Option<DisplayDeviceId> {
        self.live_tickets
            .lock()
            .ok()
            .and_then(|tickets| tickets.get(token).map(|ticket| ticket.device.clone()))
    }

    /// The capabilities an enrolled device declared.
    pub(crate) fn device_capabilities(
        &self,
        device: &DisplayDeviceId,
    ) -> Option<ReceiverCapabilities> {
        self.store.snapshot().ok().and_then(|state| {
            state
                .devices
                .get(device.as_str())
                .map(|record| record.capabilities.clone())
        })
    }

    /// What a surface can show in one Orbit, asked of the World.
    ///
    /// `Ok(None)` is a surface that lists nothing — its own answer, not a
    /// failure. The query runs under the same query-only host a render does,
    /// so listing reaches no further than drawing.
    pub async fn surface_choices(
        &self,
        orbit: &str,
        world: &WorldId,
        surface: &DisplaySurfaceId,
    ) -> Result<Option<Vec<DisplayChoice>>> {
        let package = self
            .registry
            .package_for_world(world)
            .ok_or_else(|| anyhow!("display World is not declared by a selected runner"))?;
        let registered = package
            .display_surface(surface)
            .ok_or_else(|| anyhow!("display surface is not declared by the selected runner"))?;
        let Some(invocation) = registered.choices_prepare().map_err(adapter_failure)? else {
            return Ok(None);
        };
        package
            .validate_invocation(&invocation)
            .map_err(adapter_failure)?;
        if invocation.access() != ClientAccess::Query
            || !matches!(
                invocation.kind(),
                ClientInvocationKind::World(_)
                    | ClientInvocationKind::Find { .. }
                    | ClientInvocationKind::Remote(_)
            )
        {
            return Err(anyhow!(
                "display surface did not prepare a read-only listing"
            ));
        }
        let resolved = self
            .router
            .resolve(orbit)
            .context("resolve display Orbit")?;
        let route = ControlRoute::World {
            address: resolved.address,
            world: world.as_str().to_string(),
        };
        let host = QueryOnlyHost {
            router: self.router.as_ref(),
            route,
            world,
            local_root: &self.local_root,
        };
        if package
            .confirmation(&host, &invocation)
            .await
            .map_err(adapter_failure)?
            .is_some()
        {
            return Err(anyhow!(
                "display listing unexpectedly requires interactive confirmation"
            ));
        }
        let value = package
            .execute(&host, invocation)
            .await
            .map_err(adapter_failure)?;
        registered
            .choices_project(value)
            .map(Some)
            .map_err(adapter_failure)
    }

    /// What to render, independent of who is going to look at it.
    ///
    /// An assignment supplies this for an attached receiver; a member screen
    /// supplies it directly. Everything downstream — the surface contract, the
    /// required-Query classification, the disposable renderer and its bounds —
    /// is identical, because none of it was ever about the receiver.
    async fn render_surface(
        &self,
        want: &SurfaceRender,
        pin: Option<&AssignmentRecord>,
    ) -> Result<DisplayProjection> {
        let package = self
            .registry
            .package_for_world(&want.world)
            .ok_or_else(|| anyhow!("display World is not declared by a selected runner"))?;
        let surface = package
            .display_surface(&want.surface)
            .ok_or_else(|| anyhow!("display surface is not declared by the selected runner"))?;
        surface
            .descriptor
            .validate(&want.world)
            .map_err(adapter_failure)
            .context("validate display surface descriptor")?;
        if let Some(assignment) = pin {
            validate_source_pin(assignment, surface)?;
        }

        let reviewed = self
            .router
            .reviewed_world_implementation(&want.world)
            .ok_or_else(|| anyhow!("display World has no host implementation"))?;
        if let Some(assignment) = pin {
            if reviewed != assignment.source.implementation {
                return Err(anyhow!(
                    "display assignment implementation does not match the daemon's reviewed implementation"
                ));
            }
        }

        let resolved = self
            .router
            .resolve(&want.orbit)
            .context("resolve display Orbit")?;
        if let Some(space) = want.space.as_deref() {
            if resolved.address.space.as_str() != space {
                return Err(anyhow!("display Space does not match its resolved Orbit"));
            }
        }

        let request = DisplayRequest {
            surface: want.surface.clone(),
            width: want.width,
            height: want.height,
            scale_milli: want.scale_milli,
            theme: want.theme,
            locale: want.locale.clone(),
            window_start_unix: want.now_unix_ms / 1_000,
            window_horizon_ms: want.horizon_ms,
            input: want.input.clone(),
        };
        request.validate().map_err(adapter_failure)?;
        let invocation = surface.prepare(&request).map_err(adapter_failure)?;
        package
            .validate_invocation(&invocation)
            .map_err(adapter_failure)?;
        // Process-backed packages keep their parsed operation opaque, so a
        // legitimate World/Find query arrives as `Remote`. It is still bounded
        // by `QueryOnlyHost` below: cross-World calls, Runtime Work, control,
        // content, and every mutation-capable facility are refused at the
        // callback boundary rather than trusted to the runner's classification.
        if invocation.access() != ClientAccess::Query
            || !matches!(
                invocation.kind(),
                ClientInvocationKind::World(_)
                    | ClientInvocationKind::Find { .. }
                    | ClientInvocationKind::Remote(_)
            )
        {
            return Err(anyhow!(
                "display surface did not prepare a read-only World, Find, or remote invocation"
            ));
        }

        let content_route = ControlRoute::Orbit {
            address: resolved.address.clone(),
        };
        let home = resolved.home.clone();
        let route = ControlRoute::World {
            address: resolved.address,
            world: want.world.as_str().to_string(),
        };
        let host = QueryOnlyHost {
            router: self.router.as_ref(),
            route,
            world: &want.world,
            local_root: &self.local_root,
        };
        if package
            .confirmation(&host, &invocation)
            .await
            .map_err(adapter_failure)?
            .is_some()
        {
            return Err(anyhow!(
                "display query unexpectedly requires interactive confirmation"
            ));
        }
        let value = package
            .execute(&host, invocation)
            .await
            .map_err(adapter_failure)?;
        let projection = surface
            .project(value, &request)
            .await
            .map_err(adapter_failure)?;
        projection
            .validate_for(&surface.descriptor, &request)
            .map_err(adapter_failure)?;
        self.resolve_stored_frames(projection, &home, &content_route)
            .await
    }

    /// A `StoredFrame` names bytes the World has only the record of. They are
    /// fetched here, once per render and bounded, and checked to be the still
    /// they claim. What cannot be fetched blanks its own item with a reason —
    /// one bad entry is one blank, not a program nobody can compile.
    async fn resolve_stored_frames(
        &self,
        mut projection: DisplayProjection,
        home: &Path,
        route: &ControlRoute,
    ) -> Result<DisplayProjection> {
        for item in &mut projection.program.items {
            let RenderedScene::StoredFrame(stored) = &item.scene else {
                continue;
            };
            let resource = data_encoding::HEXLOWER.encode(stored.content.as_bytes());
            item.scene = match self
                .fetch_stored_frame(home, route, &resource, stored)
                .await
            {
                Ok(frame) => RenderedScene::Frame(frame),
                Err(error) => {
                    tracing::warn!(
                        resource = %resource,
                        error = %format_args!("{error:#}"),
                        "stored display frame could not be served"
                    );
                    RenderedScene::Blank(BlankReason::SourceUnavailable)
                }
            };
        }
        Ok(projection)
    }

    async fn fetch_stored_frame(
        &self,
        home: &Path,
        route: &ControlRoute,
        resource: &str,
        stored: &StoredFrame,
    ) -> Result<RenderedFrame> {
        let total = match crate::control::content_call(
            home,
            &crate::control::content_request(
                route.clone(),
                crate::control::ContentCall::Stat {
                    content: resource.to_string(),
                },
            ),
        )
        .await
        {
            Ok((crate::control::ContentReply::ContentStatus { plaintext_len, .. }, _)) => {
                plaintext_len
            }
            Ok((reply, _)) => return Err(anyhow!("stored frame stat refused: {reply:?}")),
            Err(error) => return Err(error).context("stat stored frame"),
        };
        let bound = u64::try_from(MAX_RENDERED_ASSET_BYTES).unwrap_or(u64::MAX);
        if total == 0 || total > bound {
            return Err(anyhow!("stored frame is {total} bytes, outside its bound"));
        }
        let bytes = self.read_stored(home, route, resource, 0, total).await?;
        let held = sniff_still(&bytes)
            .ok_or_else(|| anyhow!("stored frame bytes are not a PNG, JPEG or WebP"))?;
        if held != stored.media_type {
            return Err(anyhow!(
                "stored frame declares {:?} but holds {held:?}",
                stored.media_type
            ));
        }
        // The dimensions a receiver checks are the ones it decodes, and what
        // it decodes is these bytes — so they are read from the bytes, not
        // from the catalog's memory of the upload. A record that disagrees is
        // noted and overruled; refusing it would blank a still whose only
        // fault is a stale number beside it.
        let (width, height) = still_dimensions(held, &bytes)
            .ok_or_else(|| anyhow!("stored frame header does not carry its dimensions"))?;
        if (width, height) != (stored.width, stored.height) {
            tracing::warn!(
                resource = %resource,
                declared = %format_args!("{}x{}", stored.width, stored.height),
                held = %format_args!("{width}x{height}"),
                "stored display frame is not the size its record says; serving the bytes' own size"
            );
        }
        Ok(RenderedFrame {
            media_type: held,
            width,
            height,
            bytes,
        })
    }

    /// Render a surface for a screen that is a **member of the Space**, not an
    /// attached receiver.
    ///
    /// Nothing between here and the World changes. What is absent is everything
    /// that exists because a receiver is a stranger: no pairing, no proof key,
    /// no assignment record, no opaque asset handle, and no
    /// [`ProgramCompiler`] — a member already holds the Space these bytes came
    /// from, so binding them to a credential it does not have would protect
    /// nothing.
    ///
    /// Selection is therefore local and revocation is convergent: losing
    /// standing stops the Query, with no policy to push and nothing to
    /// clawback.
    pub async fn render_for_member(&self, want: &SurfaceRender) -> Result<DisplayProjection> {
        self.render_surface(want, None).await
    }

    /// The hub resource a device's whole-program stream lives under. Stable
    /// per assignment: the receiver's URL names it for as long as the
    /// assignment lasts.
    fn program_resource(assignment: &AssignmentRecord) -> String {
        format!("prog-{}", assignment.id.as_str())
    }

    fn producer_for(&self, assignment: &DisplayAssignmentId) -> Option<Arc<Producer>> {
        self.producers
            .lock()
            .ok()
            .and_then(|producers| producers.get(assignment.as_str()).cloned())
    }

    fn retire_producer(&self, assignment: &DisplayAssignmentId) {
        if let Some(producer) = self
            .producers
            .lock()
            .ok()
            .and_then(|mut producers| producers.remove(assignment.as_str()))
        {
            producer.retire();
        }
    }

    /// Where local World registrations live for this identity.
    pub(crate) fn identity_root(&self) -> PathBuf {
        self.router.catalog().identity().to_path_buf()
    }

    /// The state of every producer, by assignment id: `running`, `idle` (a
    /// screen nobody fetched from; resumes on its next poll) or `retired`.
    pub(crate) fn producer_states(&self) -> Vec<(String, &'static str)> {
        self.producers
            .lock()
            .map(|producers| {
                producers
                    .iter()
                    .map(|(assignment, producer)| {
                        let state = if !producer.is_active() {
                            "retired"
                        } else if producer.running.load(Ordering::SeqCst) {
                            "running"
                        } else {
                            "idle"
                        };
                        (assignment.clone(), state)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// When an assignment's stream was last asked for by any receiver, or
    /// `None` when its presentation does not exist in this process.
    pub(crate) fn stream_served_at(&self, assignment: &AssignmentRecord) -> Option<u64> {
        let orbit = super::live::assignment_orbit_key(&assignment.space, &assignment.orbit);
        self.live
            .fetched_at(&orbit, &Self::program_resource(assignment))
            .filter(|&at| at > 0)
    }

    /// The producer for this assignment on this receiver, made if there is
    /// none or the one there serves a different shape of screen.
    fn producer_for_receiver(
        &self,
        assignment: &AssignmentRecord,
        capabilities: &ReceiverCapabilities,
        now_unix_ms: u64,
    ) -> Result<Arc<Producer>> {
        if let Some(producer) = self.producer_for(&assignment.id) {
            if producer.serves(assignment, capabilities) && producer.is_active() {
                return Ok(producer);
            }
            self.retire_producer(&assignment.id);
        }
        let resolved = self.router.resolve(&assignment.orbit)?;
        let memory = {
            let mut epochs = self
                .producer_epochs
                .lock()
                .map_err(|_| anyhow!("display producer epoch lock was poisoned"))?;
            *epochs
                .entry(assignment.id.as_str().to_string())
                .or_insert(ProducerMemory {
                    epoch_unix_ms: now_unix_ms,
                    discontinuities_dropped: 0,
                    window_first_sequence: None,
                })
        };
        self.persist_epochs();
        let stored_epoch_unix_ms = Some(memory.epoch_unix_ms);
        let producer = Arc::new(Producer {
            assignment: assignment.clone(),
            capabilities: capabilities.clone(),
            orbit_key: super::live::assignment_orbit_key(&assignment.space, &assignment.orbit),
            resource: Self::program_resource(assignment),
            reads: ContentReads {
                home: resolved.home.clone(),
                route: ControlRoute::Orbit {
                    address: resolved.address,
                },
            },
            started_at_unix_ms: now_unix_ms,
            stored_epoch_unix_ms,
            resumed_discontinuities_dropped: memory.discontinuities_dropped,
            resumed_window_first_sequence: memory.window_first_sequence,
            state: tokio::sync::Mutex::new(ProducerState {
                timeline: None,
                cache: StillCache::default(),
                next_render_unix_ms: u64::MAX,
            }),
            running: AtomicBool::new(false),
            active: AtomicBool::new(true),
            wake: Notify::new(),
        });
        self.producers
            .lock()
            .map_err(|_| anyhow!("display producer lock was poisoned"))?
            .insert(assignment.id.as_str().to_string(), producer.clone());
        Ok(producer)
    }

    /// Have the producer's task running, if it is not.
    fn start_producer(self: &Arc<Self>, producer: &Arc<Producer>) {
        if producer
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tokio::spawn(run_keeper(Arc::downgrade(self), producer.clone()));
            tokio::spawn(run_renderer(Arc::downgrade(self), producer.clone()));
        }
    }

    /// Resolve, query, render, validate, and freeze the current assignment for
    /// one enrolled receiver. `now_unix_ms` is supplied by the lifecycle so
    /// expiry and the rendered time window share one clock reading.
    ///
    /// A native-HLS receiver plays the whole program as one endless stream a
    /// producer keeps made at the live edge; its program on the wire is one
    /// open-ended item that never changes, so this compiles it without
    /// rendering once the producer is up. The producer renders on the World's
    /// own cadence and doorbell. A program that cannot be one stream — a live
    /// source — compiles to the per-item program, which still works.
    pub async fn compile_for_device(
        self: &Arc<Self>,
        device: &DisplayDeviceId,
        capabilities: &ReceiverCapabilities,
        now_unix_ms: u64,
    ) -> Result<Arc<CompiledProgram>> {
        validate_capabilities(capabilities).context("validate receiver capabilities")?;
        let state = self.store.snapshot()?;
        let enrolled = state
            .devices
            .get(device.as_str())
            .ok_or_else(|| anyhow!("display device is not enrolled"))?;
        if enrolled.revoked_at_unix_ms.is_some() {
            return Err(anyhow!("display device enrollment is revoked"));
        }
        let assignment = state
            .assignments
            .values()
            .find(|assignment| {
                &assignment.device == device && assignment.revoked_at_unix_ms.is_none()
            })
            .cloned()
            .ok_or_else(|| anyhow!("display device has no active assignment"))?;
        validate_assignment(&assignment, now_unix_ms)?;

        let native_hls = matches!(
            capabilities.playback.tier,
            PlaybackTier::NativeHls | PlaybackTier::NativeFull
        );
        tracing::debug!(
            device = %device,
            assignment = %assignment.id,
            tier = ?capabilities.playback.tier,
            "compiling display program"
        );
        let program_resource = Self::program_resource(&assignment);
        let stream = |frame: Option<(u32, u32)>| -> Result<CompiledProgram> {
            self.compiler.compile_stream(
                &assignment.id,
                &assignment.program,
                assignment.freshness.clone(),
                &program_resource,
                frame,
                None,
            )
        };
        let mut streaming = self
            .producer_for(&assignment.id)
            .filter(|producer| native_hls && producer.serves(&assignment, capabilities))
            .filter(|producer| producer.is_active());
        if let Some(producer) = &streaming {
            if !producer.has_program().await {
                streaming = None;
            }
        }
        let compiled = if let Some(producer) = streaming {
            producer.ensure_window(&self.live, now_unix_ms).await?;
            self.start_producer(&producer);
            stream(producer.frame().await)?
        } else {
            let world = WorldId::parse(&assignment.source.world)
                .ok_or_else(|| anyhow!("display assignment pins an invalid World"))?;
            let want = SurfaceRender {
                orbit: assignment.orbit.clone(),
                space: Some(assignment.space.clone()),
                world,
                surface: assignment.source.surface.clone(),
                input: assignment.source.input.clone(),
                theme: assignment.theme,
                width: capabilities.viewport.width,
                height: capabilities.viewport.height,
                scale_milli: capabilities.viewport.scale_milli,
                locale: capabilities.locale.clone(),
                horizon_ms: capabilities.max_staging_horizon_ms,
                now_unix_ms,
            };
            let projection = self.render_surface(&want, Some(&assignment)).await?;
            let unstreamable = if native_hls {
                producer::unstreamable(&projection)
            } else {
                Some("receiver plays no native HLS")
            };
            match unstreamable {
                None => {
                    let producer =
                        self.producer_for_receiver(&assignment, capabilities, now_unix_ms)?;
                    match producer.feed(&self.live, &projection, now_unix_ms).await {
                        Ok(_) => {
                            self.start_producer(&producer);
                            stream(producer.frame().await)?
                        }
                        Err(error) => {
                            // A schedule that could not be made — a clip that
                            // would not read, a still that would not encode —
                            // is the per-item program for now, and said.
                            tracing::warn!(
                                device = %device,
                                error = %format_args!("{error:#}"),
                                "program stream could not be scheduled; serving the per-item program"
                            );
                            self.retire_producer(&assignment.id);
                            self.live.remove_stored(
                                &super::live::assignment_orbit_key(
                                    &assignment.space,
                                    &assignment.orbit,
                                ),
                                &program_resource,
                            );
                            let alignment = playback_alignment(&state, &assignment, now_unix_ms)?;
                            self.compiler.compile(
                                &assignment.id,
                                &assignment.program,
                                assignment.freshness.clone(),
                                projection,
                                alignment.as_ref(),
                                capabilities.playback.tier,
                            )?
                        }
                    }
                }
                Some(reason) => {
                    tracing::debug!(device = %device, reason, "serving the per-item program");
                    self.retire_producer(&assignment.id);
                    let alignment = playback_alignment(&state, &assignment, now_unix_ms)?;
                    self.compiler.compile(
                        &assignment.id,
                        &assignment.program,
                        assignment.freshness.clone(),
                        projection,
                        alignment.as_ref(),
                        capabilities.playback.tier,
                    )?
                }
            }
        };
        let compiled = Arc::new(compiled);
        validate_receiver_fit(compiled.as_ref(), capabilities)?;
        let previous = self
            .compiled
            .lock()
            .map_err(|_| anyhow!("display program cache lock was poisoned"))?
            .insert(device.as_str().to_string(), compiled.clone());
        if previous.is_some_and(|previous| previous.program.revision != compiled.program.revision) {
            if let Ok(mut changes) = self.revision_changes.lock() {
                changes.insert(device.as_str().to_string(), now_unix_ms);
            }
        }
        Ok(compiled)
    }

    pub fn current_program(&self, device: &DisplayDeviceId) -> Result<Option<DisplayProgram>> {
        Ok(self
            .compiled
            .lock()
            .map_err(|_| anyhow!("display program cache lock was poisoned"))?
            .get(device.as_str())
            .map(|compiled| compiled.program.clone()))
    }

    pub fn active_assignment_for_device(
        &self,
        device: &DisplayDeviceId,
        now_unix_ms: u64,
    ) -> Result<Option<AssignmentRecord>> {
        Ok(self
            .store
            .assignment_for_device(device)?
            .filter(|assignment| validate_assignment(assignment, now_unix_ms).is_ok()))
    }

    pub fn subscribe_changes(&self) -> DisplayChangeSubscriptions {
        DisplayChangeSubscriptions {
            assignments: self.assignment_changes.subscribe(),
            worlds: self.router.subscribe(),
        }
    }

    pub fn notify_assignment_change(&self) {
        let _ = self.assignment_changes.send(());
    }

    /// Wait until either controller state changes, the assigned Orbit reports
    /// a relevant World invalidation/reset, or the receiver's bounded wait
    /// expires. `true` means a controller or relevant World doorbell fired;
    /// `false` means only the timer elapsed. This is a doorbell, never a patch.
    pub async fn wait_for_change(
        &self,
        assignment: &AssignmentRecord,
        mut subscriptions: DisplayChangeSubscriptions,
        wait: Duration,
    ) -> bool {
        let orbit = assignment.orbit.as_str();
        let world = assignment.source.world.as_str();
        let changed = async {
            loop {
                tokio::select! {
                    assignment = subscriptions.assignments.recv() => {
                        let _ = assignment;
                        return;
                    }
                    doorbell = subscriptions.worlds.recv() => {
                        let Ok(doorbell) = doorbell else { return };
                        if doorbell.orbit.as_str() != orbit {
                            continue;
                        }
                        if doorbell.doorbell.reset
                            || doorbell
                                .doorbell
                                .invalidations
                                .iter()
                                .any(|invalidation| invalidation.world.as_str() == world)
                        {
                            return;
                        }
                    }
                }
            }
        };
        tokio::time::timeout(wait, changed).await.is_ok()
    }

    /// Re-sample one already compiled program on its persisted group clock.
    ///
    /// A timer-only long-poll wakeup changes playback position, not World
    /// semantics or assets. Keeping that distinction here prevents members of
    /// one sync group from queueing identical full renders behind the same
    /// World runner at the boundary they are meant to share.
    pub fn aligned_playback_for(
        &self,
        assignment: &AssignmentRecord,
        program: &DisplayProgram,
        sampled_at_unix_ms: u64,
    ) -> Result<display_protocol::program::DisplayPlayback> {
        let state = self.store.snapshot()?;
        let alignment = playback_alignment(&state, assignment, sampled_at_unix_ms)?
            .ok_or_else(|| anyhow!("display assignment has no sync alignment"))?;
        super::compiler::aligned_playback(&program.items, program.playback.cycle, &alignment)
            .map(|(playback, _)| playback)
    }

    pub fn current_asset(
        &self,
        device: &DisplayDeviceId,
        asset: &DisplayAssetId,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .compiled
            .lock()
            .map_err(|_| anyhow!("display program cache lock was poisoned"))?
            .get(device.as_str())
            .and_then(|compiled| compiled.asset(asset))
            .map(<[u8]>::to_vec))
    }

    pub fn current_media_resource(
        &self,
        device: &DisplayDeviceId,
        manifest: &DisplayAssetId,
    ) -> Result<Option<String>> {
        Ok(self
            .compiled
            .lock()
            .map_err(|_| anyhow!("display program cache lock was poisoned"))?
            .get(device.as_str())
            .and_then(|compiled| compiled.media_resource(manifest))
            .map(str::to_string))
    }

    pub(crate) async fn mint_live_ticket(
        &self,
        device: &DisplayDeviceId,
        current_item: &DisplayProgramItemId,
        manifest: &DisplayAssetId,
        transport: LiveTransport,
        now_unix_ms: u64,
    ) -> Result<LiveTicketGrant> {
        let assignment = self
            .active_assignment_for_device(device, now_unix_ms)?
            .ok_or_else(|| anyhow!("display device has no active assignment"))?;
        let compiled = self
            .compiled
            .lock()
            .map_err(|_| anyhow!("display program cache lock was poisoned"))?
            .get(device.as_str())
            .cloned()
            .ok_or_else(|| anyhow!("display program is not compiled"))?;
        let item = compiled
            .program
            .items
            .iter()
            .find(|item| &item.id == current_item)
            .ok_or_else(|| anyhow!("live ticket item is not in the current program"))?;
        let (scene_transport, live) = match &item.scene {
            DisplayScene::Media {
                manifest: scene_manifest,
                protocol,
                live,
            } if &scene_manifest.id == manifest => (
                match protocol {
                    display_protocol::program::MediaProtocol::Mse => LiveTransport::Mse,
                    display_protocol::program::MediaProtocol::Hls => LiveTransport::Hls,
                    display_protocol::program::MediaProtocol::Dash => {
                        return Err(anyhow!("DASH fanout is not supported"))
                    }
                },
                *live,
            ),
            _ => return Err(anyhow!("manifest is not the current media item")),
        };
        if scene_transport != transport {
            return Err(anyhow!("live ticket transport does not match the program"));
        }
        match transport {
            LiveTransport::Mse
                if !matches!(
                    self.device_playback_tier(device)?,
                    PlaybackTier::MseLive | PlaybackTier::NativeFull
                ) =>
            {
                return Err(anyhow!("receiver did not declare MSE live playback"))
            }
            LiveTransport::Hls
                if !matches!(
                    self.device_playback_tier(device)?,
                    PlaybackTier::NativeHls | PlaybackTier::NativeFull
                ) =>
            {
                return Err(anyhow!("receiver did not declare native HLS playback"))
            }
            _ => {}
        }
        let resource = compiled
            .media_resource(manifest)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("live manifest has no coordinator resource"))?;
        let resolved = self
            .router
            .resolve(&assignment.orbit)
            .context("resolve live display Orbit")?;
        if resolved.address.space.as_str() != assignment.space {
            return Err(anyhow!("live assignment Space changed"));
        }
        let orbit = super::live::assignment_orbit_key(&assignment.space, &assignment.orbit);
        if live {
            // A live source is dialled on demand and takes a moment to announce
            // its catalog, so waiting is the right thing to do.
            self.live
                .ensure_orbit(self.router.clone(), resolved.address)
                .await?;
            let ready = async {
                while !self.live.has_resource(&orbit, &resource, transport) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            };
            tokio::time::timeout(LIVE_SOURCE_WAIT, ready)
                .await
                .context("live media source did not publish its catalog")?;
        } else if !self.live.has_resource(&orbit, &resource, transport) {
            // A finite source is installed from its own bytes, once. The
            // resource *is* the content id — that is what the compiler writes
            // for a stored origin — so nothing product-specific is consulted:
            // the coordinator reads the container's table of contents through
            // the content plane and plans every segment without touching the
            // film.
            self.install_stored(&resolved.home, &assignment, &orbit, &resource)
                .await
                .with_context(|| format!("stored media {resource} would not install"))?;
        }
        let expires_at_unix_ms = now_unix_ms
            .checked_add(ticket_lifetime_ms(transport))
            .ok_or_else(|| anyhow!("live ticket expiry overflowed"))?;
        let token = random_token()?;
        let mut tickets = self
            .live_tickets
            .lock()
            .map_err(|_| anyhow!("display live ticket lock was poisoned"))?;
        tickets.retain(|_, ticket| {
            if ticket.expires_at_unix_ms <= now_unix_ms {
                return false;
            }
            !ticket
                .superseded_at_unix_ms
                .is_some_and(|since| !within_restage_grace(Some(since), now_unix_ms))
        });
        // The device's earlier tickets are superseded, not dropped: its player
        // is still fetching on them while the receiver swaps over.
        for ticket in tickets.values_mut() {
            if &ticket.device == device && ticket.superseded_at_unix_ms.is_none() {
                ticket.superseded_at_unix_ms = Some(now_unix_ms);
            }
        }
        if tickets.len() >= MAX_LIVE_TICKETS {
            return Err(anyhow!("display live ticket bound reached"));
        }
        tickets.insert(
            token.clone(),
            LiveTicket {
                device: device.clone(),
                assignment: compiled.program.assignment.clone(),
                program: compiled.program.program.clone(),
                revision: compiled.program.revision.clone(),
                current_item: current_item.clone(),
                manifest: manifest.clone(),
                orbit,
                resource,
                transport,
                expires_at_unix_ms,
                superseded_at_unix_ms: None,
            },
        );
        drop(tickets);
        self.persist_tickets();
        Ok(LiveTicketGrant {
            token,
            expires_at_unix_ms,
        })
    }

    pub(crate) fn authorize_live_ticket(
        &self,
        token: &str,
        transport: LiveTransport,
        consume: bool,
        now_unix_ms: u64,
    ) -> Result<AuthorizedLiveStream> {
        let ticket = {
            let mut tickets = self
                .live_tickets
                .lock()
                .map_err(|_| anyhow!("display live ticket lock was poisoned"))?;
            take_ticket(&mut tickets, token, transport, consume, now_unix_ms)?
        };
        self.validate_live_stream(&ticket, now_unix_ms)?;
        Ok(AuthorizedLiveStream {
            orbit: ticket.orbit.clone(),
            resource: ticket.resource.clone(),
            ticket,
        })
    }

    /// Whether this receiver holds an unexpired ticket for a native-HLS
    /// stream. Tickets live in memory, so after a daemon restart a receiver
    /// playing a stream program still holds a URL nothing will answer — and
    /// because the stream program's revision is stable, its poll would say
    /// "no change" forever while its player hammers a dead ticket. The poll
    /// checks this and answers with a restart reset instead.
    pub(crate) fn device_holds_hls_ticket(
        &self,
        device: &DisplayDeviceId,
        now_unix_ms: u64,
    ) -> bool {
        self.live_tickets.lock().ok().is_some_and(|tickets| {
            tickets.values().any(|ticket| {
                &ticket.device == device
                    && ticket.transport == LiveTransport::Hls
                    && ticket.expires_at_unix_ms > now_unix_ms
            })
        })
    }

    pub(crate) fn live_stream_still_authorized(
        &self,
        stream: &AuthorizedLiveStream,
        now_unix_ms: u64,
    ) -> bool {
        self.validate_live_stream_assignment(&stream.ticket, now_unix_ms)
            .is_ok()
    }

    pub(crate) fn live_hub(&self) -> &LiveMediaHub {
        &self.live
    }

    fn validate_live_stream(&self, ticket: &LiveTicket, now_unix_ms: u64) -> Result<()> {
        if now_unix_ms >= ticket.expires_at_unix_ms {
            return Err(anyhow!("live ticket expired"));
        }
        self.validate_live_stream_assignment(ticket, now_unix_ms)
    }

    fn validate_live_stream_assignment(&self, ticket: &LiveTicket, now_unix_ms: u64) -> Result<()> {
        let assignment = self
            .active_assignment_for_device(&ticket.device, now_unix_ms)?
            .filter(|assignment| {
                assignment.id == ticket.assignment && assignment.program == ticket.program
            })
            .ok_or_else(|| anyhow!("live assignment was revoked or replaced"))?;
        if super::live::assignment_orbit_key(&assignment.space, &assignment.orbit) != ticket.orbit {
            return Err(anyhow!("live assignment Orbit changed"));
        }
        let compiled = self
            .compiled
            .lock()
            .map_err(|_| anyhow!("display program cache lock was poisoned"))?
            .get(ticket.device.as_str())
            .cloned()
            .ok_or_else(|| anyhow!("live program is unavailable"))?;
        // The origin was fixed when the ticket was minted; what this re-checks
        // every request is that the program has not been revised out from under
        // it. Requiring `live: true` here would refuse a stored ticket on its
        // first segment for a reason that has nothing to do with the program.
        let current = compiled.program.revision == ticket.revision
            && compiled.program.items.iter().any(|item| {
                item.id == ticket.current_item
                    && matches!(
                        &item.scene,
                        DisplayScene::Media { manifest, .. } if manifest.id == ticket.manifest
                    )
            });
        if current {
            return Ok(());
        }
        let changed_at = self
            .revision_changes
            .lock()
            .ok()
            .and_then(|changes| changes.get(ticket.device.as_str()).copied());
        if within_restage_grace(changed_at, now_unix_ms)
            || within_restage_grace(ticket.superseded_at_unix_ms, now_unix_ms)
        {
            return Ok(());
        }
        Err(anyhow!("media program was revised"))
    }

    /// Install a stored content as a planned presentation.
    ///
    /// The reads go over the daemon's own content channel with patience, so a
    /// content this Station has the name of and not the bytes is fetched from
    /// a peer by the same supply every other read uses. What is read here is
    /// the box headers and the `moov` — the table of contents — never the
    /// `mdat`; the film's bytes move later, one asked-for segment at a time.
    async fn install_stored(
        &self,
        home: &std::path::Path,
        assignment: &AssignmentRecord,
        orbit: &str,
        resource: &str,
    ) -> Result<()> {
        let route = crate::control::ControlRoute::Orbit {
            address: crate::daemon::OrbitAddress {
                space: mechanics::ids::SpaceId::parse(&assignment.space)
                    .context("stored assignment names an unparseable Space")?,
                orbit: crate::daemon::LocalOrbitId::parse(&assignment.orbit)
                    .context("stored assignment names an unparseable Orbit")?,
            },
        };
        let total = match crate::control::content_call(
            home,
            &crate::control::content_request(
                route.clone(),
                crate::control::ContentCall::Stat {
                    content: resource.to_string(),
                },
            ),
        )
        .await
        {
            Ok((crate::control::ContentReply::ContentStatus { plaintext_len, .. }, _)) => {
                plaintext_len
            }
            Ok((reply, _)) => return Err(anyhow!("stored content stat refused: {reply:?}")),
            Err(error) => return Err(error).context("stat stored content"),
        };

        let moov = self.fetch_moov(home, &route, resource, total).await?;
        let policy = mediabox::CatalogPolicy {
            max_group_duration_ms: runtime::plane::live::media::DEFAULT_MAX_GROUP_DURATION_MS,
            target_latency_ms: runtime::plane::live::media::DEFAULT_MAX_LATENCY_MS,
            jitter_hint_ms: 50,
            // The rendition a ticket resolves against is the resource itself,
            // which for a stored origin is the content id.
            rendition: resource.to_string(),
        };
        let plan = mediabox::StoredPlan::from_moov(&moov, &policy)
            .map_err(|error| anyhow!("stored content would not plan: {error}"))?;
        self.live.install_planned(orbit, resource, plan)
    }

    /// Walk the container's top-level boxes and return the whole `moov`.
    ///
    /// The walk is [`mediabox::box_header`] over sixteen-byte reads, so a
    /// `moov` written after a gigabyte of `mdat` — where a camera puts one —
    /// costs a handful of header reads and one bounded body read.
    async fn fetch_moov(
        &self,
        home: &std::path::Path,
        route: &crate::control::ControlRoute,
        resource: &str,
        total: u64,
    ) -> Result<Vec<u8>> {
        let reads = ContentReads {
            home: home.to_path_buf(),
            route: route.clone(),
        };
        producer::fetch_moov(&reads, resource, total).await
    }

    /// One bounded stored read, looped over the channel's range ceiling.
    async fn read_stored(
        &self,
        home: &std::path::Path,
        route: &crate::control::ControlRoute,
        resource: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>> {
        read_stored(home, route, resource, offset, len).await
    }

    /// One HLS segment, from wherever this presentation keeps it.
    ///
    /// A live presentation holds its window materialised and answers from it.
    /// A planned one holds a table: the segment's byte ranges are answered off
    /// the hub lock, read here — fetching from a peer if this Station lacks
    /// them — and packaged by a fresh muxer. The film is never resident as
    /// segments; each one exists for the life of one response.
    /// Make sure the program a ticket names is compiled and, for a stream,
    /// producing — for a ticket minted by a previous run of this daemon.
    /// The receiver kept its URL across the restart; this is what makes the
    /// URL answer again without the receiver having to notice.
    pub(crate) async fn revive_ticket(self: &Arc<Self>, token: &str, now_unix_ms: u64) {
        let Some(device) = self.ticket_device(token) else {
            return;
        };
        let compiled = self
            .compiled
            .lock()
            .ok()
            .is_some_and(|compiled| compiled.contains_key(device.as_str()));
        let streaming = self
            .active_assignment_for_device(&device, now_unix_ms)
            .ok()
            .flatten()
            .is_some_and(|assignment| {
                let orbit = super::live::assignment_orbit_key(&assignment.space, &assignment.orbit);
                self.live.has_resource(
                    &orbit,
                    &Self::program_resource(&assignment),
                    LiveTransport::Hls,
                )
            });
        if compiled && streaming {
            return;
        }
        let Some(capabilities) = self.device_capabilities(&device) else {
            return;
        };
        match self
            .compile_for_device(&device, &capabilities, now_unix_ms)
            .await
        {
            Ok(compiled) => {
                // The stream came back a different stream (its frame changed
                // while this process was down): the ticket cannot be honoured,
                // so the receiver's poll is woken to re-stage now rather than
                // when its wait runs out.
                let stale = self
                    .live_tickets
                    .lock()
                    .ok()
                    .and_then(|tickets| tickets.get(token).map(|ticket| ticket.revision.clone()))
                    .is_some_and(|revision| revision != compiled.program.revision);
                if stale {
                    // The receiver holds the revision before; give its ticket
                    // the same grace an in-process change would, and wake its
                    // poll so it re-stages now.
                    if let Ok(mut changes) = self.revision_changes.lock() {
                        changes.insert(device.as_str().to_string(), now_unix_ms);
                    }
                    self.notify_assignment_change();
                }
            }
            Err(error) => {
                tracing::debug!(device = %device, error = %format_args!("{error:#}"), "a ticket from before this process could not be revived");
            }
        }
    }

    pub(crate) async fn hls_segment(
        &self,
        stream: &AuthorizedLiveStream,
        sequence: u64,
        now_unix_ms: u64,
    ) -> Result<Vec<u8>> {
        self.live
            .touch(&stream.orbit, &stream.resource, now_unix_ms);
        if let Ok(bytes) =
            self.live
                .hls_segment(&stream.orbit, &stream.resource, &stream.resource, sequence)
        {
            return Ok(bytes);
        }
        let (plan, segment) =
            self.live
                .planned_segment(&stream.orbit, &stream.resource, sequence)?;
        let assignment = self
            .active_assignment_for_device(&stream.ticket.device, now_unix_ms)?
            .ok_or_else(|| anyhow!("stored assignment is gone"))?;
        let resolved = self
            .router
            .resolve(&assignment.orbit)
            .context("resolve stored display Orbit")?;
        let route = crate::control::ControlRoute::Orbit {
            address: resolved.address.clone(),
        };
        let mut bytes = Vec::with_capacity(segment.ranges.len());
        for (offset, size) in &segment.ranges {
            bytes.push(
                self.read_stored(
                    &resolved.home,
                    &route,
                    &stream.resource,
                    *offset,
                    u64::from(*size),
                )
                .await?,
            );
        }
        super::LiveMediaHub::package_planned(&plan, sequence, &bytes)
    }

    /// The MSE packets for one planned segment, or `None` past the film's end.
    ///
    /// The same read `hls_segment`'s planned half performs — ranges off the
    /// hub lock, bytes through the content plane, fetched from a peer when
    /// this Station lacks them — packaged by a fresh CMAF muxer instead of a
    /// TS one. `None` is the end saying so, distinct from every failure.
    pub(crate) async fn mse_planned_segment(
        &self,
        stream: &AuthorizedLiveStream,
        sequence: u64,
        now_unix_ms: u64,
    ) -> Result<Option<Vec<super::LiveMediaPacket>>> {
        let Some(plan) = self.live.planned_for_mse(&stream.orbit, &stream.resource) else {
            return Err(anyhow!("this presentation is not planned"));
        };
        let index = usize::try_from(sequence).map_err(|_| anyhow!("segment sequence overflow"))?;
        let Some(segment) = plan.plan(index) else {
            return Ok(None);
        };
        let assignment = self
            .active_assignment_for_device(&stream.ticket.device, now_unix_ms)?
            .ok_or_else(|| anyhow!("stored assignment is gone"))?;
        let resolved = self
            .router
            .resolve(&assignment.orbit)
            .context("resolve stored display Orbit")?;
        let route = crate::control::ControlRoute::Orbit {
            address: resolved.address.clone(),
        };
        let mut bytes = Vec::with_capacity(segment.ranges.len());
        for (offset, size) in &segment.ranges {
            bytes.push(
                self.read_stored(
                    &resolved.home,
                    &route,
                    &stream.resource,
                    *offset,
                    u64::from(*size),
                )
                .await?,
            );
        }
        super::LiveMediaHub::package_planned_mse(&plan, sequence, &bytes).map(Some)
    }

    fn device_playback_tier(&self, device: &DisplayDeviceId) -> Result<PlaybackTier> {
        self.store
            .device(device)?
            .map(|record| record.capabilities.playback.tier)
            .ok_or_else(|| anyhow!("display device is not enrolled"))
    }
}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).context("mint display live ticket")?;
    Ok(data_encoding::HEXLOWER.encode(&bytes))
}

fn playback_alignment(
    state: &CoordinatorPolicy,
    assignment: &AssignmentRecord,
    sampled_at_unix_ms: u64,
) -> Result<Option<PlaybackAlignment>> {
    let Some(sync) = &assignment.sync else {
        return Ok(None);
    };
    let mut effective_mode = sync.mode;
    if effective_mode == DisplaySyncMode::Positional {
        for member in state.assignments.values().filter(|member| {
            member.revoked_at_unix_ms.is_none()
                && member
                    .expires_at_unix_ms
                    .is_none_or(|expires| sampled_at_unix_ms < expires)
                && member
                    .sync
                    .as_ref()
                    .is_some_and(|member| member.group == sync.group)
        }) {
            let device = state
                .devices
                .get(member.device.as_str())
                .ok_or_else(|| anyhow!("display sync group member is not enrolled"))?;
            if !matches!(
                device.capabilities.playback.sync_class,
                SyncClass::PositionalA | SyncClass::PositionalB
            ) {
                effective_mode = DisplaySyncMode::StayInSync;
                break;
            }
        }
    }
    Ok(Some(PlaybackAlignment {
        group: sync.group.clone(),
        mode: effective_mode,
        epoch_unix_ms: sync.epoch_unix_ms,
        sampled_at_unix_ms,
        static_delay_ms: sync.static_delay_ms,
    }))
}

/// The lifetime a ticket on this transport is granted, and re-granted.
const fn ticket_lifetime_ms(transport: LiveTransport) -> u64 {
    match transport {
        LiveTransport::Mse => MSE_TICKET_LIFETIME_MS,
        LiveTransport::Hls => HLS_TICKET_LIFETIME_MS,
    }
}

/// Find the ticket a request presents, dropping every expired one on the way.
///
/// A ticket that is used **slides**: each authorised use re-grants the full
/// lifetime from now. The receiver holds one stream URL for the life of its
/// assignment and never re-mints while it is playing, so a fixed expiry would
/// 403 a healthy screen a day after it was switched on, for no reason a person
/// could see. What bounds a ticket is the assignment it names — revocation and
/// replacement are re-checked on every request by the caller — and a screen
/// that stops fetching lets its ticket lapse on its own. A consumed ticket
/// (the MSE socket, which answers exactly once) is removed rather than slid.
fn take_ticket(
    tickets: &mut BTreeMap<String, LiveTicket>,
    token: &str,
    transport: LiveTransport,
    consume: bool,
    now_unix_ms: u64,
) -> Result<LiveTicket> {
    tickets.retain(|_, ticket| {
        ticket.expires_at_unix_ms > now_unix_ms
            && !ticket
                .superseded_at_unix_ms
                .is_some_and(|since| !within_restage_grace(Some(since), now_unix_ms))
    });
    let ticket = tickets
        .get_mut(token)
        .filter(|ticket| ticket.transport == transport)
        .ok_or_else(|| anyhow!("live ticket is invalid or expired"))?;
    if consume {
        let ticket = ticket.clone();
        tickets.remove(token);
        return Ok(ticket);
    }
    ticket.expires_at_unix_ms = now_unix_ms
        .checked_add(ticket_lifetime_ms(transport))
        .ok_or_else(|| anyhow!("live ticket expiry overflowed"))?;
    Ok(ticket.clone())
}

fn validate_assignment(assignment: &AssignmentRecord, now_unix_ms: u64) -> Result<()> {
    if assignment.protocol_major != display_protocol::PROTOCOL_MAJOR {
        return Err(anyhow!("display assignment protocol is unsupported"));
    }
    if assignment
        .expires_at_unix_ms
        .is_some_and(|expires| now_unix_ms >= expires)
    {
        return Err(anyhow!("display assignment has expired"));
    }
    Ok(())
}

fn validate_source_pin(
    assignment: &AssignmentRecord,
    surface: &world_interface::display::DisplaySurface,
) -> Result<()> {
    let descriptor = &surface.descriptor;
    if descriptor.runtime_implementation != assignment.source.implementation
        || descriptor.contract_version != assignment.source.surface_contract_version
        || descriptor.contract_digest != assignment.source.surface_contract_digest
        || Sha256::digest(assignment.source.input.as_bytes()).as_slice()
            != assignment.source.input_sha256
    {
        return Err(anyhow!(
            "display assignment source no longer matches its exact package pin"
        ));
    }
    Ok(())
}

/// A JSON file's contents, or the default when it is absent or unreadable.
fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write a JSON file atomically; a failure is logged, never fatal — the
/// state it holds is a convenience across restarts, not the authority.
fn save_json<T: serde::Serialize>(path: &Path, value: &T) {
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let temp = path.with_extension("json.tmp");
    let written = serde_json::to_vec_pretty(value)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| std::fs::write(&temp, bytes).map_err(anyhow::Error::from))
        .and_then(|()| std::fs::rename(&temp, path).map_err(anyhow::Error::from));
    if let Err(error) = written {
        tracing::debug!(path = %path.display(), %error, "display state was not persisted");
    }
}

/// One bounded stored read, looped over the channel's range ceiling.
async fn read_stored(
    home: &std::path::Path,
    route: &crate::control::ControlRoute,
    resource: &str,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>> {
    /// How long one stored range may spend fetching bytes a peer holds.
    const STORED_READ_PATIENCE_MS: u32 = 5_000;
    let ceiling =
        u64::try_from(runtime::plane::freight::content::MAX_RANGE_BYTES).unwrap_or(u64::MAX);
    let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or_default());
    let mut at = offset;
    let mut left = len;
    while left > 0 {
        let want = left.min(ceiling);
        let (reply, bytes) = crate::control::content_call(
            home,
            &crate::control::content_request(
                route.clone(),
                crate::control::ContentCall::Read {
                    content: resource.to_string(),
                    offset: at,
                    len: want,
                    patience_ms: STORED_READ_PATIENCE_MS,
                },
            ),
        )
        .await
        .context("read stored content")?;
        match reply {
            crate::control::ContentReply::ContentStream { .. } if !bytes.is_empty() => {
                let landed = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                at = at.saturating_add(landed);
                left = left.saturating_sub(landed);
                out.extend_from_slice(&bytes);
            }
            crate::control::ContentReply::ContentStream { .. } => break,
            other => return Err(anyhow!("stored read refused: {other:?}")),
        }
    }
    Ok(out)
}

fn validate_receiver_fit(
    compiled: &CompiledProgram,
    capabilities: &ReceiverCapabilities,
) -> Result<()> {
    if compiled.program.items.len() > usize::from(capabilities.max_program_items) {
        return Err(anyhow!(
            "compiled display program exceeds receiver item capacity"
        ));
    }
    let mut staged_bytes = 0u64;
    for item in &compiled.program.items {
        if let DisplayScene::Frame { asset } = &item.scene {
            if !capabilities.image_types.contains(&asset.media_type) {
                return Err(anyhow!(
                    "compiled display frame uses an image type the receiver did not declare"
                ));
            }
            if asset.encoded_len > capabilities.max_asset_bytes {
                return Err(anyhow!("compiled display asset exceeds receiver capacity"));
            }
        }
    }
    for (_, bytes) in compiled.assets() {
        staged_bytes = staged_bytes
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| anyhow!("compiled display assets exceed receiver capacity"))?;
    }
    if staged_bytes > u64::from(capabilities.max_staged_bytes) {
        return Err(anyhow!(
            "compiled display assets exceed receiver staging capacity"
        ));
    }
    Ok(())
}

fn adapter_failure(error: Failure) -> anyhow::Error {
    anyhow!(error
        .diagnostic()
        .unwrap_or("World client adapter failure")
        .to_string())
}

/// The still type the bytes themselves say they are — the declaration on the
/// record is what the uploader claimed, and a receiver checks the served
/// content type against the bytes it gets.
fn sniff_still(bytes: &[u8]) -> Option<FrameMediaType> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(FrameMediaType::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(FrameMediaType::Jpeg)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some(FrameMediaType::WebP)
    } else {
        None
    }
}

/// The pixel dimensions a still's header declares — what a decoder will
/// report, read without decoding.
fn still_dimensions(kind: FrameMediaType, bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |at: usize| -> Option<u32> {
        let window = bytes.get(at..at.checked_add(4)?)?;
        Some(u32::from_be_bytes(window.try_into().ok()?))
    };
    let le24 = |at: usize| -> Option<u32> {
        let [low, mid, high] = <[u8; 3]>::try_from(bytes.get(at..at.checked_add(3)?)?).ok()?;
        Some(u32::from_le_bytes([low, mid, high, 0]))
    };
    let le16 = |at: usize| -> Option<u32> {
        let [low, high] = <[u8; 2]>::try_from(bytes.get(at..at.checked_add(2)?)?).ok()?;
        Some(u32::from_le_bytes([low, high, 0, 0]))
    };
    let nonzero = |width: u32, height: u32| (width > 0 && height > 0).then_some((width, height));
    match kind {
        // IHDR is always the first chunk: length, "IHDR", width, height.
        FrameMediaType::Png => {
            if bytes.get(12..16) != Some(b"IHDR".as_slice()) {
                return None;
            }
            nonzero(be32(16)?, be32(20)?)
        }
        // Walk the marker segments to the first start-of-frame.
        FrameMediaType::Jpeg => {
            let mut at = 2usize;
            loop {
                let &[0xFF, marker] = bytes.get(at..at.checked_add(2)?)? else {
                    return None;
                };
                if marker == 0xFF {
                    at = at.checked_add(1)?;
                    continue;
                }
                if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
                    at = at.checked_add(2)?;
                    continue;
                }
                let length = usize::from(u16::from_be_bytes([
                    *bytes.get(at.checked_add(2)?)?,
                    *bytes.get(at.checked_add(3)?)?,
                ]));
                let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
                if is_sof {
                    let height = u32::from(u16::from_be_bytes([
                        *bytes.get(at.checked_add(5)?)?,
                        *bytes.get(at.checked_add(6)?)?,
                    ]));
                    let width = u32::from(u16::from_be_bytes([
                        *bytes.get(at.checked_add(7)?)?,
                        *bytes.get(at.checked_add(8)?)?,
                    ]));
                    return nonzero(width, height);
                }
                at = at.checked_add(2)?.checked_add(length)?;
            }
        }
        // The first chunk after the RIFF header names the bitstream form.
        FrameMediaType::WebP => match bytes.get(12..16)? {
            b"VP8X" => nonzero(le24(24)?.checked_add(1)?, le24(27)?.checked_add(1)?),
            b"VP8L" => {
                let bits = be32(21).map(u32::swap_bytes)?;
                nonzero(
                    (bits & 0x3FFF).checked_add(1)?,
                    ((bits >> 14) & 0x3FFF).checked_add(1)?,
                )
            }
            b"VP8 " => nonzero(le16(26)? & 0x3FFF, le16(28)? & 0x3FFF),
            _ => None,
        },
    }
}

struct QueryOnlyHost<'a> {
    router: &'a Router,
    route: ControlRoute,
    world: &'a WorldId,
    local_root: &'a Path,
}

impl ClientHost for QueryOnlyHost<'_> {
    fn local_root(&self) -> &Path {
        self.local_root
    }

    fn call_world<'a>(&'a self, call: Call) -> ClientFuture<'a, Reply> {
        Box::pin(async move {
            if call.world() != self.world {
                return Err(Failure::new(
                    "display projection attempted to query another World",
                ));
            }
            self.router
                .call_world_requiring(self.route.clone(), &call, REQUIRED_WORLD_ACCESS)
                .await
                .map_err(|error| Failure::new(format!("{error:#}")))
        })
    }

    fn call_find<'a>(
        &'a self,
        world: WorldId,
        query: runtime::find::Query,
    ) -> ClientFuture<'a, Value> {
        Box::pin(async move {
            if &world != self.world {
                return Err(Failure::new(
                    "display projection attempted to query another World",
                ));
            }
            let ControlRoute::World { address, .. } = &self.route else {
                return Err(Failure::new("display projection has no World route"));
            };
            let response = self
                .router
                .request_routed(
                    ControlRoute::Orbit {
                        address: address.clone(),
                    },
                    &Request::Find {
                        world: world.as_str().to_owned(),
                        query,
                    },
                    None,
                )
                .await
                .map_err(|error| Failure::new(format!("{error:#}")))?;
            match response {
                crate::control::Response::Find { answer } => serde_json::to_value(answer)
                    .map_err(|error| Failure::new(format!("encode Runtime Find answer: {error}"))),
                crate::control::Response::Error { message, .. } => Err(Failure::new(message)),
                other => Err(Failure::new(format!(
                    "Runtime Find request returned an unexpected response: {other:?}"
                ))),
            }
        })
    }

    fn call_work<'a>(&'a self, _request: runtime::exec::WorkRequest) -> ClientFuture<'a, Value> {
        Box::pin(async {
            Err(Failure::new(
                "display projections cannot execute Runtime Work",
            ))
        })
    }

    fn call_control<'a>(&'a self, _request: HostControlRequest) -> ClientFuture<'a, Value> {
        Box::pin(async {
            Err(Failure::new(
                "display projections cannot mutate host control",
            ))
        })
    }

    fn call_content<'a>(&'a self, _request: HostContentRequest) -> ClientFuture<'a, Value> {
        Box::pin(async {
            Err(Failure::new(
                "display projections cannot access local content",
            ))
        })
    }

    fn call_identity<'a>(
        &'a self,
        _handles: Vec<PresentationHandle>,
    ) -> ClientFuture<'a, PresentationResolution> {
        Box::pin(async { Ok(PresentationResolution::unavailable()) })
    }
}

#[cfg(test)]
mod tests {
    /// A resumed window's count is the recorded one moved to the resumed
    /// first sequence by the seams between: forward past skipped ones, back
    /// when the resume lands before the anchor, unchanged with no anchor.
    #[test]
    fn a_resumed_window_counts_the_seams_between_the_anchor_and_its_first() {
        // Seams at every multiple of 6.
        let before = |n: u64| (1..n).filter(|m| m % 6 == 0).count() as u64;
        assert_eq!(
            super::resumed_discontinuities(4, Some(17), 233, before),
            4 + 36
        );
        assert_eq!(super::resumed_discontinuities(4, Some(17), 20, before), 5);
        assert_eq!(super::resumed_discontinuities(4, Some(17), 17, before), 4);
        assert_eq!(super::resumed_discontinuities(4, Some(17), 11, before), 3);
        assert_eq!(super::resumed_discontinuities(4, None, 233, before), 4);
    }

    use std::collections::BTreeSet;

    #[test]
    fn a_stored_still_is_known_by_its_bytes_not_its_record() {
        use super::sniff_still;
        use world_interface::display::FrameMediaType;
        assert_eq!(
            sniff_still(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some(FrameMediaType::Png)
        );
        assert_eq!(
            sniff_still(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16]),
            Some(FrameMediaType::Jpeg)
        );
        assert_eq!(
            sniff_still(b"RIFF\x24\0\0\0WEBPVP8 "),
            Some(FrameMediaType::WebP)
        );
        // An MP4 is not a still, whatever its upload was called.
        assert_eq!(sniff_still(b"\0\0\0\x20ftypisom"), None);
        assert_eq!(sniff_still(b"RIFF"), None);
    }

    use display_protocol::ids::{DisplayAssignmentId, DisplayProgramId};
    use display_protocol::program::{FreshnessPolicy, StaleAction};
    use world_interface::display::{
        CanonicalDisplayInput, DisplayOutputKind, DisplaySurfaceDescriptor, DisplaySurfaceId,
        DisplayTheme,
    };

    use super::*;

    #[test]
    fn exact_source_pin_detects_contract_or_input_drift() {
        let world = WorldId::parse("com.example.signage").unwrap();
        let surface_id = DisplaySurfaceId::new("signage.program").unwrap();
        let mut descriptor = DisplaySurfaceDescriptor {
            id: surface_id.clone(),
            title: "Program".into(),
            runtime_implementation: [7; 32],
            contract_version: 1,
            input_contract_digest: [8; 32],
            renderer_identity: [9; 32],
            contract_digest: [0; 32],
            outputs: BTreeSet::from([DisplayOutputKind::Frame]),
        };
        descriptor.contract_digest = descriptor.expected_contract_digest(&world);
        let input = CanonicalDisplayInput::new(b"{}".to_vec()).unwrap();
        let assignment = AssignmentRecord {
            version: 1,
            id: DisplayAssignmentId::parse("11".repeat(16)).unwrap(),
            device: DisplayDeviceId::parse("22".repeat(16)).unwrap(),
            orbit: "orbit".into(),
            space: "space".into(),
            program: DisplayProgramId::parse("33".repeat(16)).unwrap(),
            source: super::super::SourceGrant::new(
                world.as_str().into(),
                [7; 32],
                surface_id,
                1,
                descriptor.contract_digest,
                input,
            ),
            controller: "controller".into(),
            coordinator_actor: "actor".into(),
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            theme: DisplayTheme::Dark,
            freshness: FreshnessPolicy {
                stale_after_ms: 10_000,
                on_stale: StaleAction::Blank,
            },
            sync: None,
            expires_at_unix_ms: None,
            revoked_at_unix_ms: None,
        };
        let surface = world_interface::display::DisplaySurface::local(
            descriptor,
            |_| unreachable!(),
            |_| unreachable!(),
            Arc::new(UnusedRenderer),
        );
        validate_source_pin(&assignment, &surface).unwrap();
        let mut drifted = assignment;
        drifted.source.surface_contract_version = 2;
        assert!(validate_source_pin(&drifted, &surface).is_err());
    }

    fn ticket(transport: LiveTransport, expires_at_unix_ms: u64) -> LiveTicket {
        LiveTicket {
            device: DisplayDeviceId::parse("22".repeat(16)).unwrap(),
            assignment: DisplayAssignmentId::parse("11".repeat(16)).unwrap(),
            program: DisplayProgramId::parse("33".repeat(16)).unwrap(),
            revision: ProgramRevision::parse("0".repeat(64)).unwrap(),
            current_item: DisplayProgramItemId::parse("44".repeat(32)).unwrap(),
            manifest: DisplayAssetId::parse("55".repeat(32)).unwrap(),
            orbit: "space/orbit".into(),
            resource: "prog-1111".into(),
            transport,
            expires_at_unix_ms,
            superseded_at_unix_ms: None,
        }
    }

    /// A ticket replaced by a newer one for the same device is honoured
    /// through the re-stage grace — the player is still fetching on it — and
    /// gone after.
    #[test]
    fn a_superseded_ticket_lives_through_the_grace_and_no_longer() {
        let mut tickets = BTreeMap::new();
        let mut old = ticket(LiveTransport::Hls, 1_000_000);
        old.superseded_at_unix_ms = Some(10_000);
        tickets.insert("old".to_string(), old);
        take_ticket(
            &mut tickets,
            "old",
            LiveTransport::Hls,
            false,
            10_000 + RESTAGE_GRACE_MS,
        )
        .unwrap();
        assert!(take_ticket(
            &mut tickets,
            "old",
            LiveTransport::Hls,
            false,
            10_001 + RESTAGE_GRACE_MS
        )
        .is_err());
        assert!(!tickets.contains_key("old"));
    }

    /// A ticket for the revision before is honoured through the grace and
    /// refused after it, so a receiver mid-re-stage is never turned away
    /// and a receiver that never re-staged does not keep an old stream.
    #[test]
    fn a_revised_tickets_grace_is_short_and_bounded() {
        assert!(!within_restage_grace(None, 1_000));
        assert!(within_restage_grace(Some(1_000), 1_000));
        assert!(within_restage_grace(Some(1_000), 1_000 + RESTAGE_GRACE_MS));
        assert!(!within_restage_grace(Some(1_000), 1_001 + RESTAGE_GRACE_MS));
    }

    /// A receiver holds one stream URL for the life of its assignment and
    /// never re-mints while playing, so the ticket behind it must outlive any
    /// fixed lifetime as long as it is in use: every authorised fetch re-grants
    /// the full lifetime from now. One that stops being used lapses on its
    /// own, and a consumed ticket (the socket's one answer) is gone, not slid.
    #[test]
    fn a_used_ticket_slides_its_expiry_and_an_unused_one_lapses() {
        let mut tickets = BTreeMap::new();
        tickets.insert("playing".to_string(), ticket(LiveTransport::Hls, 1_000));
        tickets.insert("idle".to_string(), ticket(LiveTransport::Hls, 1_000));
        tickets.insert("socket".to_string(), ticket(LiveTransport::Mse, 1_000));

        let used = take_ticket(&mut tickets, "playing", LiveTransport::Hls, false, 900).unwrap();
        assert_eq!(used.expires_at_unix_ms, 900 + HLS_TICKET_LIFETIME_MS);
        assert_eq!(
            tickets["playing"].expires_at_unix_ms,
            900 + HLS_TICKET_LIFETIME_MS,
            "the slide is recorded, not just reported"
        );
        // The wrong transport is not this ticket.
        assert!(take_ticket(&mut tickets, "playing", LiveTransport::Mse, false, 900).is_err());

        // The socket's ticket answers once and is gone.
        take_ticket(&mut tickets, "socket", LiveTransport::Mse, true, 900).unwrap();
        assert!(!tickets.contains_key("socket"));

        // Past the original lifetime, the idle ticket has lapsed and the one in
        // use has not.
        assert!(take_ticket(&mut tickets, "idle", LiveTransport::Hls, false, 5_000).is_err());
        assert!(!tickets.contains_key("idle"));
        take_ticket(&mut tickets, "playing", LiveTransport::Hls, false, 5_000).unwrap();
    }

    struct UnusedRenderer;

    impl world_interface::display::DisplayRenderer for UnusedRenderer {
        fn project<'a>(
            &'a self,
            _value: Value,
            _request: &'a DisplayRequest,
        ) -> world_interface::display::DisplayProjectFuture<'a> {
            unreachable!()
        }
    }
}
