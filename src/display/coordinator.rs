//! Durable receiver, assignment, and health coordination.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
use tokio::sync::broadcast;
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
    assignment_changes: broadcast::Sender<()>,
    live: LiveMediaHub,
    live_tickets: Mutex<BTreeMap<String, LiveTicket>>,
}

#[derive(Clone)]
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

impl DisplayCoordinator {
    pub fn new(
        store: Arc<CoordinatorStore>,
        router: Arc<Router>,
        registry: WorldClientRegistry,
        local_root: PathBuf,
    ) -> Result<Self> {
        let identifier_key = store.identifier_key()?;
        let (assignment_changes, _) = broadcast::channel(64);
        Ok(Self {
            store,
            router,
            registry,
            compiler: ProgramCompiler::new(identifier_key)?,
            local_root,
            compiled: Mutex::new(BTreeMap::new()),
            assignment_changes,
            live: LiveMediaHub::default(),
            live_tickets: Mutex::new(BTreeMap::new()),
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
        Ok(RenderedFrame {
            media_type: held,
            width: stored.width,
            height: stored.height,
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

    /// Resolve, query, render, validate, and freeze the current assignment for
    /// one enrolled receiver. `now_unix_ms` is supplied by the lifecycle so
    /// expiry and the rendered time window share one clock reading.
    pub async fn compile_for_device(
        &self,
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
        let alignment = playback_alignment(&state, &assignment, now_unix_ms)?;
        tracing::debug!(
            device = %device,
            assignment = %assignment.id,
            tier = ?capabilities.playback.tier,
            "compiling display program"
        );
        let compiled = Arc::new(self.compiler.compile(
            &assignment.id,
            &assignment.program,
            assignment.freshness,
            projection,
            alignment.as_ref(),
            capabilities.playback.tier,
        )?);
        validate_receiver_fit(compiled.as_ref(), capabilities)?;
        self.compiled
            .lock()
            .map_err(|_| anyhow!("display program cache lock was poisoned"))?
            .insert(device.as_str().to_string(), compiled.clone());
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
        let lifetime = match transport {
            LiveTransport::Mse => MSE_TICKET_LIFETIME_MS,
            LiveTransport::Hls => HLS_TICKET_LIFETIME_MS,
        };
        let expires_at_unix_ms = now_unix_ms
            .checked_add(lifetime)
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
            if &ticket.device != device {
                return true;
            }
            ticket.assignment == compiled.program.assignment
                && ticket.program == compiled.program.program
                && ticket.revision == compiled.program.revision
                && (ticket.current_item != *current_item || ticket.transport != transport)
        });
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
            },
        );
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
            tickets.retain(|_, ticket| ticket.expires_at_unix_ms > now_unix_ms);
            let ticket = tickets
                .get(token)
                .filter(|ticket| ticket.transport == transport)
                .cloned()
                .ok_or_else(|| anyhow!("live ticket is invalid or expired"))?;
            if consume {
                tickets.remove(token);
            }
            ticket
        };
        self.validate_live_stream(&ticket, now_unix_ms)?;
        Ok(AuthorizedLiveStream {
            orbit: ticket.orbit.clone(),
            resource: ticket.resource.clone(),
            ticket,
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
        if compiled.program.revision != ticket.revision
            || !compiled.program.items.iter().any(|item| {
                item.id == ticket.current_item
                    && matches!(
                        &item.scene,
                        DisplayScene::Media { manifest, .. } if manifest.id == ticket.manifest
                    )
            })
        {
            return Err(anyhow!("media program was revised"));
        }
        Ok(())
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
        const HEADER: u32 = 16;
        /// A `moov` is a table of contents; bound it like one.
        const MAX_MOOV: u64 = 8 * 1024 * 1024;

        let mut at = 0u64;
        while at < total {
            let header = self
                .read_stored(home, route, resource, at, u64::from(HEADER))
                .await?;
            let (size, kind, _) = mediabox::box_header(&header, total, at)
                .map_err(|error| anyhow!("stored container refused: {error}"))?;
            if &kind == b"moov" {
                if size > MAX_MOOV {
                    return Err(anyhow!("stored moov exceeds its bound"));
                }
                return self.read_stored(home, route, resource, at, size).await;
            }
            at = at
                .checked_add(size)
                .ok_or_else(|| anyhow!("stored container box overflows"))?;
        }
        Err(anyhow!("stored content has no moov"))
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

    /// One HLS segment, from wherever this presentation keeps it.
    ///
    /// A live presentation holds its window materialised and answers from it.
    /// A planned one holds a table: the segment's byte ranges are answered off
    /// the hub lock, read here — fetching from a peer if this Station lacks
    /// them — and packaged by a fresh muxer. The film is never resident as
    /// segments; each one exists for the life of one response.
    pub(crate) async fn hls_segment(
        &self,
        stream: &AuthorizedLiveStream,
        sequence: u64,
        now_unix_ms: u64,
    ) -> Result<Vec<u8>> {
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
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(FrameMediaType::WebP)
    } else {
        None
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
