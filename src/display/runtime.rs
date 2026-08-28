//! Composition of the display coordinator with the identity daemon.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use display_protocol::bounds::MAX_STATIC_DELAY_MS;
use display_protocol::ids::{
    DisplayAssignmentId, DisplayDeviceId, DisplayPairingId, DisplayProgramId, RendezvousId,
};
use display_protocol::pairing::{CoordinatorTrust, PairingRejectionReason};
use display_protocol::program::{
    validate_sync_group, DisplaySyncMode, FreshnessPolicy, StaleAction,
};
use replica::body::WorldId;
use tokio::sync::watch;
use world_interface::display::{DisplaySurfaceId, DisplayTheme};
use world_interface::WorldClientRegistry;

use super::{
    serve_display_https, AssignmentIntent, CoordinatorStore, DisplayCoordinator, DisplayHttpState,
    DisplayPairingService, DisplayTlsIdentity, EnrollmentHook, RendezvousView,
};
use super::{AssignmentRecord, AssignmentSync, Custodian, SourceGrant};

/// How far ahead a member screen asks a surface to resolve.
///
/// An attached receiver negotiates this, because it has to stage assets before
/// it can show them across an outage. A member screen re-asks whenever it wants
/// to and is never offline from its own Space, so the horizon only has to cover
/// one comfortable presentation window.
const MEMBER_HORIZON_MS: u32 = 300_000;

/// The shortest passphrase that may hold the identifier key.
///
/// A floor rather than a composition rule. The device slot is bound to this
/// machine and cannot be attacked from elsewhere; a passphrase slot is portable
/// by design, which is the point of it and also the reason a memorable-but-short
/// one would quietly become the weakest way in.
const MIN_PASSPHRASE_CHARS: usize = 12;

/// Project a rendered surface for a member screen.
///
/// Assessment is carried through rather than folded into the items: an empty
/// program and an unavailable source are different facts, and a surface that
/// showed them the same way would be the false-disconnection defect wearing
/// display clothes.
fn present_view(
    world: &str,
    surface: &str,
    projection: world_interface::display::DisplayProjection,
) -> crate::control::DisplayPresentationView {
    use crate::control::{
        DisplayPresentationItemView, DisplayPresentationSceneView, DisplayPresentationView,
    };
    use world_interface::display::{
        BlankReason, DisplayAssessment, DisplayPartialReason, FrameMediaType, ProgramCycle,
        RenderedScene,
    };

    // Spelled out rather than derived from serde naming: these strings are a
    // wire vocabulary a surface reads, and tying them to a representation
    // nobody declared would let a rename in `world-interface` change what the
    // screen is told without anything failing.
    fn reason_name(reason: &DisplayPartialReason) -> String {
        match reason {
            DisplayPartialReason::ProvisionalData => "provisional_data",
            DisplayPartialReason::CorruptRecords => "corrupt_records",
            DisplayPartialReason::IncompleteProjection => "incomplete_projection",
            DisplayPartialReason::DegradedSource => "degraded_source",
        }
        .to_string()
    }

    fn assessed(assessment: &DisplayAssessment) -> (String, Vec<String>) {
        match assessment {
            DisplayAssessment::Current => ("current".into(), Vec::new()),
            DisplayAssessment::Partial(reasons) => {
                ("partial".into(), reasons.iter().map(reason_name).collect())
            }
            DisplayAssessment::Unavailable => ("unavailable".into(), Vec::new()),
        }
    }

    let (assessment, partial_reasons) = assessed(&projection.assessment);
    DisplayPresentationView {
        world: world.to_string(),
        surface: surface.to_string(),
        assessment,
        partial_reasons,
        cycle: match projection.program.cycle {
            ProgramCycle::HoldLast => "hold_last",
            ProgramCycle::Loop => "loop",
            ProgramCycle::PollAtEnd => "poll_at_end",
            ProgramCycle::BlankAtEnd => "blank_at_end",
        }
        .to_string(),
        refresh_after_ms: projection.program.refresh_after_ms,
        items: projection
            .program
            .items
            .into_iter()
            .map(|item| {
                let (item_assessment, _) = assessed(&item.assessment);
                DisplayPresentationItemView {
                    id: item.id,
                    duration_ms: item.duration_ms,
                    assessment: item_assessment,
                    spoken_summary: item.spoken_summary,
                    scene: match item.scene {
                        RenderedScene::Frame(frame) => DisplayPresentationSceneView::Frame {
                            media_type: match frame.media_type {
                                FrameMediaType::Png => "png",
                                FrameMediaType::Jpeg => "jpeg",
                                FrameMediaType::WebP => "webp",
                            }
                            .to_string(),
                            width: frame.width,
                            height: frame.height,
                            bytes_base64: data_encoding::BASE64.encode(&frame.bytes),
                        },
                        RenderedScene::Blank(reason) => DisplayPresentationSceneView::Blank {
                            reason: match reason {
                                BlankReason::SourceUnavailable => "source_unavailable",
                                BlankReason::Unsupported => "unsupported",
                                BlankReason::ProgramEnded => "program_ended",
                            }
                            .to_string(),
                        },
                        // Live media terminates at the coordinator's own edge,
                        // which a member screen does not run. Refuse visibly
                        // rather than draw something else.
                        RenderedScene::Media(_) => DisplayPresentationSceneView::Unsupported {
                            output: "media".into(),
                        },
                    },
                }
            })
            .collect(),
    }
}

/// The identity daemon as the coordinator's custodian.
///
/// One place builds this, so the device a slot is sealed to and the key that
/// opens it can never disagree — a mismatch would present as a coordinator that
/// mints assignments it cannot read back after a restart.
fn custodian(device_seed: &[u8; 32]) -> Custodian {
    let device = mechanics::actor::device_from_seed(device_seed);
    Custodian {
        unlock: mechanics::authorization::custody::UnlockKey::RecoveryKey {
            seed: *device_seed,
            me: device.clone(),
        },
        device,
    }
}

/// The display services owned by the identity-scoped daemon.
pub struct DisplayRuntime {
    pub store: Arc<CoordinatorStore>,
    pub coordinator: Arc<DisplayCoordinator>,
    pub pairing: Arc<DisplayPairingService>,
    pub tls: Arc<DisplayTlsIdentity>,
    /// The identity's kinship profile — the anchor receivers pair against.
    profile: mechanics::kinship::ProfileId,
    /// How this daemon opens its own identifier envelope.
    ///
    /// Held here rather than in the store, which is the boundary the store's
    /// API draws: a caller supplies an unlock at each site that spends one, so
    /// reading policy can never acquire key material. The daemon already holds
    /// this seed — it is the identity it runs as — so keeping it beside the
    /// display services adds no reach, and it is what lets an operator admit a
    /// second unlock path without handing one in from outside the process.
    custodian: Custodian,
    router: Arc<crate::orbits::Router>,
    registry: WorldClientRegistry,
    /// The label this identity publishes its route under, when it does. It is
    /// half of what a television is told: the site resolves the coordinator,
    /// the code proves the person at the controller meant this one.
    site: Option<String>,
}

impl DisplayRuntime {
    /// `device_seed` is the identity daemon's own seed. It is the coordinator's
    /// first custodian: the identifier key is sealed to the device that seed
    /// names, so losing the operating-system profile costs the machine's
    /// convenience unlock and not the key, and a restore onto another profile
    /// with the same identity opens it.
    pub fn open(
        root: &Path,
        router: Arc<crate::orbits::Router>,
        registry: WorldClientRegistry,
        device_seed: &[u8; 32],
        profile: mechanics::kinship::ProfileId,
        port: u16,
        site: Option<String>,
    ) -> Result<Self> {
        let mut identifier_key = [0u8; 32];
        getrandom::fill(&mut identifier_key).context("mint display identifier key")?;
        let store_root = root.join("state");
        let custodian = custodian(device_seed);
        let store = Arc::new(CoordinatorStore::open(
            &store_root,
            identifier_key,
            &custodian,
        )?);
        let wire_profile = display_protocol::ids::CoordinatorProfile::parse(profile.as_str())
            .map_err(|error| {
                anyhow::anyhow!("identity profile does not fit the wire: {error:?}")
            })?;
        let tls = Arc::new(DisplayTlsIdentity::load_or_create(
            &root.join("tls"),
            "Astrolabe",
            wire_profile,
            port,
        )?);
        let coordinator = Arc::new(DisplayCoordinator::new(
            store.clone(),
            router.clone(),
            registry.clone(),
            root.join("package-state"),
        )?);
        // What a rendezvous does with the receiver it enrols: the same
        // assignment path the controller's own request takes, with the
        // device that now exists.
        let keep_promise: EnrollmentHook = {
            let store = store.clone();
            let coordinator = coordinator.clone();
            let router = router.clone();
            let registry = registry.clone();
            Arc::new(move |device: &DisplayDeviceId, intent: &AssignmentIntent| {
                assign(
                    &store,
                    &coordinator,
                    &router,
                    &registry,
                    device.clone(),
                    intent.clone(),
                )
                .map(|_| ())
            })
        };
        let pairing = Arc::new(
            DisplayPairingService::new(
                store.clone(),
                tls.instance().clone(),
                tls.fingerprint().clone(),
            )?
            .with_enrollment_hook(keep_promise),
        );
        Ok(Self {
            store,
            coordinator,
            pairing,
            tls,
            custodian,
            router,
            registry,
            profile,
            site,
        })
    }

    /// Admit another device of this identity into the coordinator's custody,
    /// and hand back the sealed envelope it imports.
    ///
    /// This is placement in one act: the identifier key is re-wrapped to the
    /// recipient — never exposed, never re-encrypted, nothing already
    /// delivered is invalidated — and what leaves is the envelope, which is
    /// only as good as the slot the recipient can open. The same act with a
    /// recovery device as the recipient is the printed-key ceremony's
    /// substance; which device is being admitted is the caller's meaning, not
    /// this method's.
    ///
    /// What this deliberately does not do is admit the device into the
    /// kinship log: a second placement can *serve* with this, but a route
    /// publication the registry accepts is still signed by a genesis device
    /// until device-join lands in the reach plane — the same seam
    /// `correspondence`'s own pinned test names as the next piece of work.
    pub fn admit_placement(&self, recipient: &mechanics::ids::DeviceId) -> Result<Vec<u8>> {
        self.store.admit_identifier_slot(
            &self.custodian.unlock,
            &mechanics::authorization::custody::SlotSpec::RecoveryKey {
                recipient: recipient.clone(),
            },
        )?;
        self.store.export_identifier()
    }

    /// The identity this coordinator answers for — what a receiver anchors on.
    ///
    /// A property of the identity, never of this placement: every placement of
    /// one identity reports the same profile, which is what lets a receiver
    /// follow the coordinator across machines without re-pairing.
    #[must_use]
    pub fn profile(&self) -> &mechanics::kinship::ProfileId {
        &self.profile
    }

    /// Serve on a listener the caller already took.
    ///
    /// The daemon binds the port to decide whether it can host displays at all, and
    /// then hands the listener here. It used to drop it and let this rebind, which
    /// made the probe a guess: anything taking the port in between arrived as a
    /// bind failure on the serving path, where the degradation ladder does not run,
    /// so the daemon died on precisely the condition the ladder exists for.
    pub async fn serve_on(
        &self,
        listener: tokio::net::TcpListener,
        stop: watch::Receiver<bool>,
    ) -> Result<()> {
        crate::display::serve_display_on(
            listener,
            DisplayHttpState {
                coordinator: self.coordinator.clone(),
                pairing: self.pairing.clone(),
            },
            self.tls.clone(),
            stop,
        )
        .await
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

    /// Async because one request — [`Request::DisplayPresent`] — reaches the
    /// World, and the member render path is the same async path an attached
    /// receiver's compilation takes. Everything else here answers from durable
    /// state and completes immediately.
    pub async fn handle_control(
        &self,
        request: &crate::control::Request,
    ) -> Option<crate::control::Response> {
        use crate::control::{Request, Response};

        if let Request::DisplayPresent {
            orbit,
            world,
            surface,
            input,
            theme,
            width,
            height,
            scale_milli,
            locale,
        } = request
        {
            let rendered = self
                .present_for_member(
                    orbit,
                    world,
                    surface,
                    input.clone(),
                    *theme,
                    *width,
                    *height,
                    *scale_milli,
                    locale.clone(),
                )
                .await
                .map(|view| Response::DisplayPresentation(Box::new(view)));
            return Some(rendered.unwrap_or_else(|error| Response::err(format!("{error:#}"))));
        }

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
                sync,
                expires_at_unix_ms,
            } => self
                .put_assignment(
                    device,
                    AssignmentIntent {
                        orbit: orbit.clone(),
                        world: world.clone(),
                        surface: surface.clone(),
                        input: input.clone(),
                        theme: *theme,
                        stale_after_ms: *stale_after_ms,
                        on_stale: *on_stale,
                        sync: sync.clone(),
                        expires_at_unix_ms: *expires_at_unix_ms,
                    },
                )
                .map(|(assignment, program)| Response::Ok {
                    message: Some(format!(
                        "assigned display {assignment} to receiver program {program}"
                    )),
                }),
            Request::DisplayRendezvousMint { label, assignment } => self
                .mint_rendezvous(label, assignment.clone())
                .map(|view| Response::DisplayRendezvous(Box::new(view))),
            Request::DisplayRendezvousRevoke { rendezvous } => {
                self.revoke_rendezvous(rendezvous).map(|()| Response::Ok {
                    message: Some(format!("revoked display rendezvous {rendezvous}")),
                })
            }
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
            Request::DisplayIdentifierAdmitPassphrase { passphrase } => self
                .admit_identifier_passphrase(passphrase)
                .map(|()| Response::Ok {
                    message: Some("the identifier key now opens with this passphrase".into()),
                }),
            _ => return None,
        };
        Some(result.unwrap_or_else(|error| Response::err(format!("{error:#}"))))
    }

    /// Render one surface for this machine's own screen.
    ///
    /// The controller supplies the World, the surface and the input; the daemon
    /// supplies the Orbit resolution and the required-Query classification, as
    /// it does for an assignment. What it deliberately does *not* supply is a
    /// receiver — nothing is stored, and the answer is good for exactly the
    /// call that asked for it.
    #[allow(clippy::too_many_arguments)]
    async fn present_for_member(
        &self,
        orbit: &str,
        world: &str,
        surface: &str,
        input: serde_json::Value,
        theme: crate::control::DisplayThemeSetting,
        width: u32,
        height: u32,
        scale_milli: u16,
        locale: String,
    ) -> Result<crate::control::DisplayPresentationView> {
        let world_id = WorldId::parse(world).context("parse display World")?;
        let surface_id = DisplaySurfaceId::new(surface.to_string())
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let package = self
            .registry
            .package_for_world(&world_id)
            .context("display World is not declared by a selected runner")?;
        let registered = package
            .display_surface(&surface_id)
            .context("display surface is not declared by the selected runner")?;
        // The package canonicalizes its own input exactly once, here as at
        // assignment: the generic path never normalizes arbitrary JSON.
        let canonical = registered
            .canonicalize_input(input)
            .map_err(|error| anyhow::anyhow!("{error}"))
            .context("canonicalize display input")?;

        let want = super::SurfaceRender {
            orbit: orbit.to_string(),
            space: None,
            world: world_id,
            surface: surface_id,
            input: canonical,
            theme: match theme {
                crate::control::DisplayThemeSetting::Light => DisplayTheme::Light,
                crate::control::DisplayThemeSetting::Dark => DisplayTheme::Dark,
                crate::control::DisplayThemeSetting::HighContrast => DisplayTheme::HighContrast,
            },
            width,
            height,
            scale_milli,
            locale,
            horizon_ms: MEMBER_HORIZON_MS,
            now_unix_ms: mechanics::wallclock::now_millis(),
        };
        let projection = self.coordinator.render_for_member(&want).await?;
        Ok(present_view(world, surface, projection))
    }

    /// Add a passphrase slot to the identifier envelope.
    ///
    /// The salt is fresh per slot, and the cost parameters are the module's
    /// defaults rather than this call's opinion — they are stored in the slot,
    /// so a package written today still opens after the defaults are raised.
    fn admit_identifier_passphrase(&self, passphrase: &str) -> Result<()> {
        // A short passphrase is worse than none here: it is a portable slot, so
        // unlike the device slot it can be attacked away from this machine.
        if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
            anyhow::bail!(
                "a passphrase protecting the identifier key must be at least \
                 {MIN_PASSPHRASE_CHARS} characters"
            );
        }
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).context("obtain passphrase salt")?;
        self.store.admit_identifier_slot(
            &self.custodian.unlock,
            &mechanics::authorization::custody::SlotSpec::Passphrase {
                passphrase: passphrase.to_owned(),
                salt,
                params: mechanics::authorization::custody::Argon2Params::default(),
            },
        )
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
            CoordinatorTrust::Profile { origin, profile } => {
                (origin.clone(), profile.as_str().to_string())
            }
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
                sync: assignment.sync.as_ref().map(|sync| {
                    crate::control::DisplayAssignmentSyncView {
                        group: sync.group.clone(),
                        mode: control_sync_mode(sync.mode),
                        static_delay_ms: sync.static_delay_ms,
                    }
                }),
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
        let pending_rendezvous = self
            .pairing
            .outstanding_rendezvous(mechanics::wallclock::now_millis())?
            .into_iter()
            .map(|minted| self.rendezvous_view(minted))
            .collect();
        let custody = self.store.identifier_custody()?;
        Ok(DisplayCoordinatorView {
            instance: self.tls.instance().instance.clone(),
            label: self.tls.instance().label.clone(),
            coordinator_profile: Some(self.profile.as_str().to_string()),
            origin,
            certificate_sha256: fingerprint,
            certificate_pem: self.tls.certificate_pem().to_string(),
            surfaces,
            devices,
            assignments,
            pending_pairings,
            pending_rendezvous,
            identifier_custody: Some(crate::control::DisplayIdentifierCustodyView {
                slots: custody.slots,
                portable: custody.portable,
            }),
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

    fn put_assignment(
        &self,
        device: &str,
        intent: AssignmentIntent,
    ) -> Result<(DisplayAssignmentId, DisplayProgramId)> {
        let device = DisplayDeviceId::parse(device.to_string()).context("parse display device")?;
        assign(
            &self.store,
            &self.coordinator,
            &self.router,
            &self.registry,
            device,
            intent,
        )
    }

    /// Mint a code for a television to enter.
    ///
    /// A promised assignment is resolved *now* — Orbit, World, surface and
    /// input — so a mistyped program id is refused at the desk where it was
    /// typed, not minutes later on a television that can only say "nothing
    /// to show". What is stored is the intent; the resolution is repeated
    /// when the receiver enrols, against whatever is selected then.
    fn mint_rendezvous(
        &self,
        label: &str,
        assignment: Option<crate::control::DisplayRendezvousAssignmentSetting>,
    ) -> Result<crate::control::DisplayRendezvousView> {
        let intent = assignment.map(|setting| AssignmentIntent {
            orbit: setting.orbit,
            world: setting.world,
            surface: setting.surface,
            input: setting.input,
            theme: setting.theme,
            stale_after_ms: setting.stale_after_ms,
            on_stale: setting.on_stale,
            sync: setting.sync,
            expires_at_unix_ms: setting.expires_at_unix_ms,
        });
        if let Some(intent) = &intent {
            validate_freshness(intent.stale_after_ms)?;
            resolve_source(
                &self.router,
                &self.registry,
                &intent.orbit,
                &intent.world,
                &intent.surface,
                intent.input.clone(),
            )?;
        }
        let minted = self.pairing.mint_rendezvous(
            label.to_string(),
            intent,
            mechanics::wallclock::now_millis(),
        )?;
        Ok(self.rendezvous_view(minted))
    }

    fn revoke_rendezvous(&self, rendezvous: &str) -> Result<()> {
        let rendezvous =
            RendezvousId::parse(rendezvous.to_string()).context("parse display rendezvous id")?;
        self.pairing.revoke_rendezvous(&rendezvous)
    }

    fn rendezvous_view(&self, minted: RendezvousView) -> crate::control::DisplayRendezvousView {
        crate::control::DisplayRendezvousView {
            rendezvous: minted.rendezvous.as_str().to_string(),
            code: minted.code,
            site: self.site.clone(),
            label: minted.label,
            assignment: minted.assignment.map(|intent| {
                crate::control::DisplayRendezvousAssignmentView {
                    orbit: intent.orbit,
                    world: intent.world,
                    surface: intent.surface,
                }
            }),
            created_at_unix_ms: minted.created_at_unix_ms,
            expires_at_unix_ms: minted.expires_at_unix_ms,
        }
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

/// A surface pinned to an Orbit, with its input already canonical: everything
/// an assignment commits that is not about the device or the policy.
struct ResolvedSource {
    orbit: String,
    space: String,
    source: SourceGrant,
}

fn validate_freshness(stale_after_ms: u32) -> Result<()> {
    let minimum = display_protocol::bounds::MAX_LONG_POLL_WAIT_MS
        .checked_add(display_protocol::bounds::LONG_POLL_STALE_MARGIN_MS)
        .context("derive display stale margin")?;
    if stale_after_ms <= minimum || stale_after_ms > display_protocol::bounds::MAX_STALE_AFTER_MS {
        anyhow::bail!(
            "display stale interval must exceed the long-poll window and remain within protocol bounds"
        );
    }
    Ok(())
}

/// Resolve what a controller named to what the daemon will actually serve.
///
/// The daemon derives Space, implementation and contract digests from its
/// own registry rather than accepting them from the controller, and the
/// package's canonicalizer is the only thing entitled to judge the input.
fn resolve_source(
    router: &crate::orbits::Router,
    registry: &WorldClientRegistry,
    orbit: &str,
    world: &str,
    surface: &str,
    input: serde_json::Value,
) -> Result<ResolvedSource> {
    let resolved = router.resolve(orbit).context("resolve display Orbit")?;
    let world = WorldId::parse(world).context("parse display World")?;
    let package = registry
        .package_for_world(&world)
        .context("display World is not declared by a selected runner")?;
    let surface_id = DisplaySurfaceId::new(surface.to_string()).map_err(|error| {
        anyhow::anyhow!(error.diagnostic().unwrap_or("invalid surface").to_string())
    })?;
    let surface = package
        .display_surface(&surface_id)
        .context("display surface is not declared by the selected runner")?;
    surface.descriptor.validate(&world).map_err(|error| {
        anyhow::anyhow!(error.diagnostic().unwrap_or("invalid surface").to_string())
    })?;
    let reviewed = router
        .reviewed_world_implementation(&world)
        .context("display World has no reviewed host implementation")?;
    if reviewed != surface.descriptor.runtime_implementation {
        anyhow::bail!("display surface and host implementation do not match");
    }
    let input = surface.canonicalize_input(input).map_err(|error| {
        anyhow::anyhow!(error.diagnostic().unwrap_or("invalid input").to_string())
    })?;
    Ok(ResolvedSource {
        orbit: resolved.address.orbit.as_str().to_string(),
        space: resolved.address.space.as_str().to_string(),
        source: SourceGrant::new(
            world.as_str().to_string(),
            reviewed,
            surface_id,
            surface.descriptor.contract_version,
            surface.descriptor.contract_digest,
            input,
        ),
    })
}

/// Commit an assignment for an enrolled receiver. One path for both the
/// controller's explicit assignment and the one a rendezvous promised, so
/// there is exactly one set of rules about what may be pinned.
fn assign(
    store: &CoordinatorStore,
    coordinator: &DisplayCoordinator,
    router: &crate::orbits::Router,
    registry: &WorldClientRegistry,
    device: DisplayDeviceId,
    intent: AssignmentIntent,
) -> Result<(DisplayAssignmentId, DisplayProgramId)> {
    validate_freshness(intent.stale_after_ms)?;
    let enrolled = store
        .device(&device)?
        .context("display device is not enrolled")?;
    if enrolled.revoked_at_unix_ms.is_some() {
        anyhow::bail!("display device is revoked");
    }
    let resolved = resolve_source(
        router,
        registry,
        &intent.orbit,
        &intent.world,
        &intent.surface,
        intent.input,
    )?;
    let now_unix_ms = mechanics::wallclock::now_millis();
    let sync = if let Some(setting) = intent.sync {
        validate_sync_group(&setting.group).context("validate display sync group")?;
        if !(-MAX_STATIC_DELAY_MS..=MAX_STATIC_DELAY_MS).contains(&setting.static_delay_ms) {
            anyhow::bail!("display static delay is outside its protocol bound");
        }
        let mode = wire_sync_mode(setting.mode);
        let state = store.snapshot()?;
        let mut epoch_unix_ms = None;
        for existing in state.assignments.values().filter(|existing| {
            existing.revoked_at_unix_ms.is_none()
                && existing
                    .expires_at_unix_ms
                    .is_none_or(|expires| now_unix_ms < expires)
                && existing
                    .sync
                    .as_ref()
                    .is_some_and(|existing| existing.group == setting.group)
        }) {
            let existing_sync = existing
                .sync
                .as_ref()
                .context("display sync group member lost its policy")?;
            if existing_sync.mode != mode {
                anyhow::bail!("display sync group already uses a different mode");
            }
            if existing.orbit != resolved.orbit
                || existing.space != resolved.space
                || existing.source.world != resolved.source.world
                || existing.source.implementation != resolved.source.implementation
                || existing.source.surface != resolved.source.surface
                || existing.source.surface_contract_version
                    != resolved.source.surface_contract_version
                || existing.source.surface_contract_digest
                    != resolved.source.surface_contract_digest
                || existing.source.input_sha256 != resolved.source.input_sha256
            {
                anyhow::bail!(
                    "display sync group members must pin the same surface and canonical input"
                );
            }
            epoch_unix_ms = Some(existing_sync.epoch_unix_ms);
        }
        Some(AssignmentSync {
            group: setting.group,
            mode,
            epoch_unix_ms: epoch_unix_ms.unwrap_or(now_unix_ms.max(1)),
            static_delay_ms: setting.static_delay_ms,
        })
    } else {
        None
    };
    let assignment = DisplayAssignmentId::parse(random_hex::<16>()?)?;
    let program = DisplayProgramId::parse(random_hex::<16>()?)?;
    let record = AssignmentRecord {
        version: 1,
        id: assignment.clone(),
        device,
        orbit: resolved.orbit,
        space: resolved.space,
        program: program.clone(),
        source: resolved.source,
        controller: "astrolabe:local-primary".into(),
        coordinator_actor: "primary".into(),
        protocol_major: display_protocol::PROTOCOL_MAJOR,
        theme: package_theme(intent.theme),
        freshness: FreshnessPolicy {
            stale_after_ms: intent.stale_after_ms,
            on_stale: match intent.on_stale {
                crate::control::DisplayStaleActionSetting::KeepWithNativeBanner => {
                    StaleAction::KeepWithNativeBanner
                }
                crate::control::DisplayStaleActionSetting::Blank => StaleAction::Blank,
            },
        },
        sync,
        expires_at_unix_ms: intent.expires_at_unix_ms,
        revoked_at_unix_ms: None,
    };
    store.replace_assignment_for_device(record, now_unix_ms)?;
    coordinator.notify_assignment_change();
    Ok((assignment, program))
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

fn wire_sync_mode(mode: crate::control::DisplaySyncModeSetting) -> DisplaySyncMode {
    match mode {
        crate::control::DisplaySyncModeSetting::StayInSync => DisplaySyncMode::StayInSync,
        crate::control::DisplaySyncModeSetting::Positional => DisplaySyncMode::Positional,
    }
}

fn control_sync_mode(mode: DisplaySyncMode) -> crate::control::DisplaySyncModeSetting {
    match mode {
        DisplaySyncMode::StayInSync => crate::control::DisplaySyncModeSetting::StayInSync,
        DisplaySyncMode::Positional => crate::control::DisplaySyncModeSetting::Positional,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::DisplayPresentationSceneView;
    use world_interface::display::{
        BlankReason, DisplayAssessment, DisplayPartialReason, DisplayProjection, DisplayResourceId,
        FrameMediaType, MediaProtocol, ProgramCycle, RenderedFrame, RenderedMedia, RenderedProgram,
        RenderedProgramItem, RenderedScene,
    };

    fn item(id: &str, scene: RenderedScene) -> RenderedProgramItem {
        RenderedProgramItem {
            id: id.to_string(),
            duration_ms: Some(5_000),
            scene,
            assessment: DisplayAssessment::Current,
            spoken_summary: None,
        }
    }

    fn projection(items: Vec<RenderedProgramItem>) -> DisplayProjection {
        DisplayProjection {
            program: RenderedProgram {
                items,
                cycle: ProgramCycle::Loop,
                refresh_after_ms: Some(60_000),
            },
            assessment: DisplayAssessment::Current,
            spoken_summary: None,
        }
    }

    #[test]
    fn a_frame_reaches_a_member_screen_as_bytes_and_its_declared_dimensions() {
        let view = present_view(
            "wrl_x",
            "signage.program",
            projection(vec![item(
                "one",
                RenderedScene::Frame(RenderedFrame {
                    media_type: FrameMediaType::Png,
                    width: 1920,
                    height: 1080,
                    bytes: b"not-really-a-png".to_vec(),
                }),
            )]),
        );

        assert_eq!(view.assessment, "current");
        assert_eq!(view.cycle, "loop");
        let DisplayPresentationSceneView::Frame {
            media_type,
            width,
            height,
            bytes_base64,
        } = &view.items[0].scene
        else {
            panic!("a frame did not project as a frame");
        };
        assert_eq!(media_type, "png");
        assert_eq!((*width, *height), (1920, 1080));
        assert_eq!(
            data_encoding::BASE64
                .decode(bytes_base64.as_bytes())
                .unwrap(),
            b"not-really-a-png"
        );
    }

    #[test]
    fn live_media_refuses_visibly_rather_than_drawing_something_else() {
        // The live edge is coordinator machinery a member screen does not run.
        // Dropping the item would leave a program that silently lost a scene,
        // and substituting a blank would claim the source was unavailable when
        // it is this screen that cannot draw it.
        let view = present_view(
            "wrl_x",
            "signage.program",
            projection(vec![item(
                "live",
                RenderedScene::Media(RenderedMedia {
                    protocol: MediaProtocol::Mse,
                    origin: world_interface::display::MediaOrigin::Live(
                        DisplayResourceId::new("res".to_string()).unwrap(),
                    ),
                }),
            )]),
        );

        assert_eq!(view.items.len(), 1, "an undrawable item was dropped");
        assert!(matches!(
            &view.items[0].scene,
            DisplayPresentationSceneView::Unsupported { output } if output == "media"
        ));
    }

    #[test]
    fn a_degraded_source_keeps_its_reasons_instead_of_reading_as_empty() {
        let mut degraded = projection(vec![item(
            "blank",
            RenderedScene::Blank(BlankReason::SourceUnavailable),
        )]);
        degraded.assessment = DisplayAssessment::Partial(
            [
                DisplayPartialReason::DegradedSource,
                DisplayPartialReason::ProvisionalData,
            ]
            .into_iter()
            .collect(),
        );

        let view = present_view("wrl_x", "signage.program", degraded);
        assert_eq!(view.assessment, "partial");
        assert!(view
            .partial_reasons
            .contains(&"degraded_source".to_string()));
        assert!(view
            .partial_reasons
            .contains(&"provisional_data".to_string()));
        assert!(matches!(
            &view.items[0].scene,
            DisplayPresentationSceneView::Blank { reason }
                if reason == "source_unavailable"
        ));
    }
}
