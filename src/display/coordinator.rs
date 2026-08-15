//! Durable receiver, assignment, and health coordination.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use display_protocol::ids::{DisplayAssetId, DisplayDeviceId};
use display_protocol::program::{DisplayProgram, DisplayScene};
use display_protocol::receiver::{validate_capabilities, ReceiverCapabilities};
use replica::body::WorldId;
use runtime::world::call::{Call, Reply};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use world_interface::display::{DisplayRequest, REQUIRED_WORLD_ACCESS};
use world_interface::{
    ClientAccess, ClientFuture, ClientHost, ClientInvocationKind, Failure, HostContentRequest,
    HostControlRequest, PresentationHandle, PresentationResolution, WorldClientRegistry,
};

use crate::control::ControlRoute;
use crate::orbits::Router;

use super::{AssignmentRecord, CompiledProgram, CoordinatorStore, ProgramCompiler};

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
        let identifier_key = store.snapshot()?.identifier_key;
        let (assignment_changes, _) = broadcast::channel(64);
        Ok(Self {
            store,
            router,
            registry,
            compiler: ProgramCompiler::new(identifier_key)?,
            local_root,
            compiled: Mutex::new(BTreeMap::new()),
            assignment_changes,
        })
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
        let package = self
            .registry
            .package_for_world(&world)
            .ok_or_else(|| anyhow!("display assignment World is not bundled by this build"))?;
        let surface = package
            .display_surface(&assignment.source.surface)
            .ok_or_else(|| anyhow!("display assignment surface is not bundled by this build"))?;
        surface
            .descriptor
            .validate(&world)
            .map_err(adapter_failure)
            .context("validate display surface descriptor")?;
        validate_source_pin(&assignment, surface)?;

        let reviewed = self
            .router
            .reviewed_world_implementation(&world)
            .ok_or_else(|| anyhow!("display assignment World has no host implementation"))?;
        if reviewed != assignment.source.implementation {
            return Err(anyhow!(
                "display assignment implementation does not match the daemon's reviewed implementation"
            ));
        }

        let resolved = self
            .router
            .resolve(&assignment.orbit)
            .context("resolve display assignment Orbit")?;
        if resolved.address.space.as_str() != assignment.space {
            return Err(anyhow!(
                "display assignment Space does not match its resolved Orbit"
            ));
        }

        let request = DisplayRequest {
            surface: assignment.source.surface.clone(),
            width: capabilities.viewport.width,
            height: capabilities.viewport.height,
            scale_milli: capabilities.viewport.scale_milli,
            theme: assignment.theme,
            locale: capabilities.locale.clone(),
            window_start_unix: now_unix_ms / 1_000,
            window_horizon_ms: capabilities.max_staging_horizon_ms,
            input: assignment.source.input.clone(),
        };
        request.validate().map_err(adapter_failure)?;
        let invocation = (surface.prepare)(&request).map_err(adapter_failure)?;
        package
            .validate_invocation(&invocation)
            .map_err(adapter_failure)?;
        if invocation.access() != ClientAccess::Query
            || !matches!(invocation.kind(), ClientInvocationKind::World(_))
        {
            return Err(anyhow!(
                "display surface did not prepare a read-only World invocation"
            ));
        }

        let route = ControlRoute::World {
            address: resolved.address,
            world: world.as_str().to_string(),
        };
        let host = QueryOnlyHost {
            router: self.router.as_ref(),
            route,
            world: &world,
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
            .renderer
            .project(value, &request)
            .await
            .map_err(adapter_failure)?;
        projection
            .validate_for(&surface.descriptor, &request)
            .map_err(adapter_failure)?;
        let compiled = Arc::new(self.compiler.compile(
            &assignment.id,
            &assignment.program,
            assignment.freshness,
            projection,
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
    /// expires. The caller always performs a fresh authoritative compile after
    /// this returns; this is a doorbell, never a patch.
    pub async fn wait_for_change(
        &self,
        assignment: &AssignmentRecord,
        mut subscriptions: DisplayChangeSubscriptions,
        wait: Duration,
    ) {
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
        let _ = tokio::time::timeout(wait, changed).await;
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
            expires_at_unix_ms: None,
            revoked_at_unix_ms: None,
        };
        let surface = world_interface::display::DisplaySurface {
            descriptor,
            canonicalize_input: |_| unreachable!(),
            prepare: |_| unreachable!(),
            renderer: Arc::new(UnusedRenderer),
        };
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
