//! Composition of the display coordinator with the identity daemon.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use display_protocol::ids::{
    DisplayAssignmentId, DisplayDeviceId, DisplayPairingId, DisplayProgramId,
};
use display_protocol::pairing::{CoordinatorTrust, PairingRejectionReason};
use display_protocol::program::{FreshnessPolicy, StaleAction};
use replica::body::WorldId;
use tokio::sync::watch;
use world_interface::display::{DisplaySurfaceId, DisplayTheme};
use world_interface::WorldClientRegistry;

use super::{
    serve_display_https, CoordinatorStore, DisplayCoordinator, DisplayHttpState,
    DisplayPairingService, DisplayTlsIdentity, DEFAULT_DISPLAY_PORT,
};
use super::{AssignmentRecord, SourceGrant};

/// The display services owned by the identity-scoped daemon.
pub struct DisplayRuntime {
    pub store: Arc<CoordinatorStore>,
    pub coordinator: Arc<DisplayCoordinator>,
    pub pairing: Arc<DisplayPairingService>,
    pub tls: Arc<DisplayTlsIdentity>,
    router: Arc<crate::orbits::Router>,
    registry: WorldClientRegistry,
}

impl DisplayRuntime {
    pub fn open(root: &Path, router: Arc<crate::orbits::Router>) -> Result<Self> {
        let mut identifier_key = [0u8; 32];
        getrandom::fill(&mut identifier_key).context("mint display identifier key")?;
        let store_root = root.join("state");
        let store = Arc::new(CoordinatorStore::open(&store_root, identifier_key)?);
        let tls = Arc::new(DisplayTlsIdentity::load_or_create(
            &root.join("tls"),
            "Astrolabe",
            DEFAULT_DISPLAY_PORT,
        )?);
        let registry = crate::world::client_packages().clone();
        let coordinator = Arc::new(DisplayCoordinator::new(
            store.clone(),
            router.clone(),
            registry.clone(),
            root.join("package-state"),
        )?);
        let pairing = Arc::new(DisplayPairingService::new(
            store.clone(),
            tls.instance().clone(),
            tls.fingerprint().clone(),
        )?);
        Ok(Self {
            store,
            coordinator,
            pairing,
            tls,
            router,
            registry,
        })
    }

    pub async fn serve(&self, stop: watch::Receiver<bool>) -> Result<()> {
        serve_display_https(
            DisplayHttpState {
                coordinator: self.coordinator.clone(),
                pairing: self.pairing.clone(),
            },
            self.tls.clone(),
            stop,
        )
        .await
    }

    pub fn handle_control(
        &self,
        request: &crate::control::Request,
    ) -> Option<crate::control::Response> {
        use crate::control::{Request, Response};

        let result = match request {
            Request::DisplayStatus => self.status().map(|view| Response::Display(Box::new(view))),
            Request::DisplayPairingApprove { pairing, label } => self
                .approve_pairing(pairing, label)
                .map(|device| Response::Ok {
                    message: Some(format!("approved display pairing for device {device}")),
                }),
            Request::DisplayPairingReject { pairing } => {
                self.reject_pairing(pairing).map(|()| Response::Ok {
                    message: Some("rejected display pairing".into()),
                })
            }
            Request::DisplayAssignmentPut {
                device,
                orbit,
                world,
                surface,
                input,
                theme,
                stale_after_ms,
                on_stale,
                expires_at_unix_ms,
            } => self
                .put_assignment(
                    device,
                    orbit,
                    world,
                    surface,
                    input.clone(),
                    *theme,
                    *stale_after_ms,
                    *on_stale,
                    *expires_at_unix_ms,
                )
                .map(|(assignment, program)| Response::Ok {
                    message: Some(format!(
                        "assigned display {assignment} to receiver program {program}"
                    )),
                }),
            Request::DisplayAssignmentRevoke { assignment } => {
                self.revoke_assignment(assignment).map(|()| Response::Ok {
                    message: Some(format!("revoked display assignment {assignment}")),
                })
            }
            Request::DisplayDeviceRevoke { device } => {
                self.revoke_device(device).map(|()| Response::Ok {
                    message: Some(format!("revoked display device {device}")),
                })
            }
            _ => return None,
        };
        Some(result.unwrap_or_else(|error| Response::err(format!("{error:#}"))))
    }

    fn status(&self) -> Result<crate::control::DisplayCoordinatorView> {
        use crate::control::{
            DisplayAssignmentView, DisplayCoordinatorView, DisplayDeviceView, DisplayPairingView,
            DisplaySurfaceView,
        };

        let state = self.store.snapshot()?;
        let (origin, fingerprint) = match &self.tls.instance().trust {
            CoordinatorTrust::PinnedCertificate { origin, sha256 } => {
                (origin.clone(), sha256.as_str().to_string())
            }
            CoordinatorTrust::WebPkiOrigin { origin } => (origin.clone(), String::new()),
        };
        let devices = state
            .devices
            .values()
            .map(|device| {
                let health = self
                    .pairing
                    .health(&device.device)
                    .ok()
                    .flatten()
                    .map(|health| crate::control::DisplayHealthView {
                        revision: health.revision.as_str().to_string(),
                        current_item: health.current_item.as_str().to_string(),
                        elapsed_ms: health.elapsed_ms,
                        connection: wire_name(&health.connection),
                        playback: wire_name(&health.playback),
                        last_error: wire_name(&health.last_error),
                        staged_items: health.staged_items,
                        staged_bytes: health.staged_bytes,
                        drift_residual_ms: health.drift_residual_ms,
                        correction_events: health.correction_events,
                        pipeline_unobservable: health.pipeline_unobservable,
                    });
                DisplayDeviceView {
                    device: device.device.as_str().to_string(),
                    label: device.label.clone(),
                    platform: wire_name(&device.capabilities.platform),
                    build: device.capabilities.build.clone(),
                    issued_at_unix_ms: device.issued_at_unix_ms,
                    revoked_at_unix_ms: device.revoked_at_unix_ms,
                    health,
                }
            })
            .collect();
        let surfaces = self
            .registry
            .packages()
            .flat_map(|package| {
                package
                    .display_surfaces()
                    .map(|surface| DisplaySurfaceView {
                        world: package.world().as_str().to_string(),
                        surface: surface.descriptor.id.as_str().to_string(),
                        title: surface.descriptor.title.clone(),
                        contract_version: surface.descriptor.contract_version,
                        outputs: surface.descriptor.outputs.iter().map(wire_name).collect(),
                    })
            })
            .collect();
        let assignments = state
            .assignments
            .values()
            .map(|assignment| DisplayAssignmentView {
                assignment: assignment.id.as_str().to_string(),
                device: assignment.device.as_str().to_string(),
                orbit: assignment.orbit.clone(),
                space: assignment.space.clone(),
                program: assignment.program.as_str().to_string(),
                world: assignment.source.world.clone(),
                surface: assignment.source.surface.as_str().to_string(),
                controller: assignment.controller.clone(),
                theme: control_theme(assignment.theme),
                expires_at_unix_ms: assignment.expires_at_unix_ms,
                revoked_at_unix_ms: assignment.revoked_at_unix_ms,
            })
            .collect();
        let pending_pairings = self
            .pairing
            .pending(mechanics::wallclock::now_millis())?
            .into_iter()
            .map(|pairing| DisplayPairingView {
                pairing: pairing.pairing.as_str().to_string(),
                confirmation_phrase: pairing.confirmation_phrase,
                certificate_sha256: pairing.coordinator_fingerprint.as_str().to_string(),
                platform: wire_name(&pairing.capabilities.platform),
                build: pairing.capabilities.build,
                created_at_unix_ms: pairing.created_at_unix_ms,
                expires_at_unix_ms: pairing.expires_at_unix_ms,
            })
            .collect();
        Ok(DisplayCoordinatorView {
            instance: self.tls.instance().instance.clone(),
            label: self.tls.instance().label.clone(),
            origin,
            certificate_sha256: fingerprint,
            certificate_pem: self.tls.certificate_pem().to_string(),
            surfaces,
            devices,
            assignments,
            pending_pairings,
        })
    }

    fn approve_pairing(&self, pairing: &str, label: &str) -> Result<DisplayDeviceId> {
        let pairing =
            DisplayPairingId::parse(pairing.to_string()).context("parse display pairing id")?;
        self.pairing.approve(
            &pairing,
            label.to_string(),
            mechanics::wallclock::now_millis(),
        )
    }

    fn reject_pairing(&self, pairing: &str) -> Result<()> {
        let pairing =
            DisplayPairingId::parse(pairing.to_string()).context("parse display pairing id")?;
        self.pairing
            .reject(&pairing, PairingRejectionReason::UserRejected)
    }

    #[allow(clippy::too_many_arguments)]
    fn put_assignment(
        &self,
        device: &str,
        orbit: &str,
        world: &str,
        surface: &str,
        input: serde_json::Value,
        theme: crate::control::DisplayThemeSetting,
        stale_after_ms: u32,
        on_stale: crate::control::DisplayStaleActionSetting,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<(DisplayAssignmentId, DisplayProgramId)> {
        let minimum = display_protocol::bounds::MAX_LONG_POLL_WAIT_MS
            .checked_add(display_protocol::bounds::LONG_POLL_STALE_MARGIN_MS)
            .context("derive display stale margin")?;
        if stale_after_ms <= minimum
            || stale_after_ms > display_protocol::bounds::MAX_STALE_AFTER_MS
        {
            anyhow::bail!(
                "display stale interval must exceed the long-poll window and remain within protocol bounds"
            );
        }
        let device = DisplayDeviceId::parse(device.to_string()).context("parse display device")?;
        let enrolled = self
            .store
            .device(&device)?
            .context("display device is not enrolled")?;
        if enrolled.revoked_at_unix_ms.is_some() {
            anyhow::bail!("display device is revoked");
        }
        let resolved = self
            .router
            .resolve(orbit)
            .context("resolve display Orbit")?;
        let world = WorldId::parse(world).context("parse display World")?;
        let package = self
            .registry
            .package_for_world(&world)
            .context("display World is not bundled")?;
        let surface_id = DisplaySurfaceId::new(surface.to_string()).map_err(|error| {
            anyhow::anyhow!(error.diagnostic().unwrap_or("invalid surface").to_string())
        })?;
        let surface = package
            .display_surface(&surface_id)
            .context("display surface is not bundled")?;
        surface.descriptor.validate(&world).map_err(|error| {
            anyhow::anyhow!(error.diagnostic().unwrap_or("invalid surface").to_string())
        })?;
        let reviewed = self
            .router
            .reviewed_world_implementation(&world)
            .context("display World has no reviewed host implementation")?;
        if reviewed != surface.descriptor.runtime_implementation {
            anyhow::bail!("display surface and host implementation do not match");
        }
        let input = (surface.canonicalize_input)(input).map_err(|error| {
            anyhow::anyhow!(error.diagnostic().unwrap_or("invalid input").to_string())
        })?;
        let assignment = DisplayAssignmentId::parse(random_hex::<16>()?)?;
        let program = DisplayProgramId::parse(random_hex::<16>()?)?;
        let record = AssignmentRecord {
            version: 1,
            id: assignment.clone(),
            device,
            orbit: resolved.address.orbit.as_str().to_string(),
            space: resolved.address.space.as_str().to_string(),
            program: program.clone(),
            source: SourceGrant::new(
                world.as_str().to_string(),
                reviewed,
                surface_id,
                surface.descriptor.contract_version,
                surface.descriptor.contract_digest,
                input,
            ),
            controller: "astrolabe:local-primary".into(),
            coordinator_actor: "primary".into(),
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            theme: package_theme(theme),
            freshness: FreshnessPolicy {
                stale_after_ms,
                on_stale: match on_stale {
                    crate::control::DisplayStaleActionSetting::KeepWithNativeBanner => {
                        StaleAction::KeepWithNativeBanner
                    }
                    crate::control::DisplayStaleActionSetting::Blank => StaleAction::Blank,
                },
            },
            expires_at_unix_ms,
            revoked_at_unix_ms: None,
        };
        self.store
            .replace_assignment_for_device(record, mechanics::wallclock::now_millis())?;
        self.coordinator.notify_assignment_change();
        Ok((assignment, program))
    }

    fn revoke_assignment(&self, assignment: &str) -> Result<()> {
        let assignment = DisplayAssignmentId::parse(assignment.to_string())?;
        if !self
            .store
            .revoke_assignment(&assignment, mechanics::wallclock::now_millis())?
        {
            anyhow::bail!("display assignment is unknown");
        }
        self.coordinator.notify_assignment_change();
        Ok(())
    }

    fn revoke_device(&self, device: &str) -> Result<()> {
        let device = DisplayDeviceId::parse(device.to_string())?;
        if !self
            .store
            .revoke_device(&device, mechanics::wallclock::now_millis())?
        {
            anyhow::bail!("display device is unknown");
        }
        self.coordinator.notify_assignment_change();
        Ok(())
    }
}

fn package_theme(theme: crate::control::DisplayThemeSetting) -> DisplayTheme {
    match theme {
        crate::control::DisplayThemeSetting::Light => DisplayTheme::Light,
        crate::control::DisplayThemeSetting::Dark => DisplayTheme::Dark,
        crate::control::DisplayThemeSetting::HighContrast => DisplayTheme::HighContrast,
    }
}

fn control_theme(theme: DisplayTheme) -> crate::control::DisplayThemeSetting {
    match theme {
        DisplayTheme::Light => crate::control::DisplayThemeSetting::Light,
        DisplayTheme::Dark => crate::control::DisplayThemeSetting::Dark,
        DisplayTheme::HighContrast => crate::control::DisplayThemeSetting::HighContrast,
    }
}

fn wire_name(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

fn random_hex<const N: usize>() -> Result<String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).context("obtain display assignment randomness")?;
    Ok(data_encoding::HEXLOWER.encode(&bytes))
}
