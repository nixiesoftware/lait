//! Native Astrolabe control of the self-hosted display coordinator.

use lait::control::{
    ControlRoute, DisplayAssignmentSyncSetting, DisplayCoordinatorView, DisplayPresentationView,
    DisplayRendezvousAssignmentSetting, DisplayRendezvousView, DisplayStaleActionSetting,
    DisplayThemeSetting, Request, Response,
};

use super::{Client, ClientError, ClientResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayAssignmentInput {
    pub device: String,
    pub orbit: String,
    pub world: String,
    pub surface: String,
    pub input: serde_json::Value,
    pub theme: DisplayThemeSetting,
    pub stale_after_ms: u32,
    pub on_stale: DisplayStaleActionSetting,
    pub sync: Option<DisplayAssignmentSyncSetting>,
    pub expires_at_unix_ms: Option<u64>,
}

/// What a code pins its television to once it connects: an assignment with
/// everything but the device, which does not exist until then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRendezvousAssignmentInput {
    pub orbit: String,
    pub world: String,
    pub surface: String,
    pub input: serde_json::Value,
    pub theme: DisplayThemeSetting,
    pub stale_after_ms: u32,
    pub on_stale: DisplayStaleActionSetting,
    pub sync: Option<DisplayAssignmentSyncSetting>,
    pub expires_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRendezvousInput {
    /// The name the television enrols under.
    pub label: String,
    pub assignment: Option<DisplayRendezvousAssignmentInput>,
}

impl Client {
    pub async fn display_status(&self) -> ClientResult<DisplayCoordinatorView> {
        match self.display_request(Request::DisplayStatus).await? {
            Response::Display(view) => Ok(*view),
            other => Err(ClientError::internal(format!(
                "unexpected display status reply: {other:?}"
            ))),
        }
    }

    pub async fn display_pairing_approve(
        &self,
        pairing: String,
        label: String,
    ) -> ClientResult<()> {
        if pairing.trim().is_empty() || label.trim().is_empty() {
            return Err(ClientError::invalid(
                "approving a display requires its pairing id and a label",
            ));
        }
        self.display_ok(Request::DisplayPairingApprove { pairing, label })
            .await
    }

    pub async fn display_pairing_reject(&self, pairing: String) -> ClientResult<()> {
        if pairing.trim().is_empty() {
            return Err(ClientError::invalid(
                "rejecting a display requires its pairing id",
            ));
        }
        self.display_ok(Request::DisplayPairingReject { pairing })
            .await
    }

    /// Mint a code for a television to enter.
    ///
    /// A promised assignment's Orbit is resolved the way an assignment's is,
    /// because a code that named a Space where the daemon wants an Orbit would
    /// fail on the television, minutes from now, instead of here.
    pub async fn display_rendezvous_mint(
        &self,
        input: DisplayRendezvousInput,
    ) -> ClientResult<DisplayRendezvousView> {
        if input.label.trim().is_empty() {
            return Err(ClientError::invalid(
                "a display code needs the name the display will enrol under",
            ));
        }
        let assignment = match input.assignment {
            None => None,
            Some(assignment) => Some(DisplayRendezvousAssignmentSetting {
                orbit: self.display_orbit(&assignment.orbit).await?,
                world: assignment.world,
                surface: assignment.surface,
                input: assignment.input,
                theme: assignment.theme,
                stale_after_ms: assignment.stale_after_ms,
                on_stale: assignment.on_stale,
                sync: assignment.sync,
                expires_at_unix_ms: assignment.expires_at_unix_ms,
            }),
        };
        match self
            .display_request(Request::DisplayRendezvousMint {
                label: input.label,
                assignment,
            })
            .await?
        {
            Response::DisplayRendezvous(view) => Ok(*view),
            other => Err(ClientError::internal(format!(
                "unexpected display rendezvous reply: {other:?}"
            ))),
        }
    }

    pub async fn display_rendezvous_revoke(&self, rendezvous: String) -> ClientResult<()> {
        if rendezvous.trim().is_empty() {
            return Err(ClientError::invalid(
                "withdrawing a display code requires its rendezvous id",
            ));
        }
        self.display_ok(Request::DisplayRendezvousRevoke { rendezvous })
            .await
    }

    pub async fn display_assignment_put(
        &self,
        mut assignment: DisplayAssignmentInput,
    ) -> ClientResult<()> {
        assignment.orbit = self.display_orbit(&assignment.orbit).await?;
        self.display_ok(Request::DisplayAssignmentPut {
            device: assignment.device,
            orbit: assignment.orbit,
            world: assignment.world,
            surface: assignment.surface,
            input: assignment.input,
            theme: assignment.theme,
            stale_after_ms: assignment.stale_after_ms,
            on_stale: assignment.on_stale,
            sync: assignment.sync,
            expires_at_unix_ms: assignment.expires_at_unix_ms,
        })
        .await
    }

    /// Resolve the Space-shaped selector the Astrolabe surface carries to the
    /// exact local Orbit address the daemon requires.
    ///
    /// Most of Astrolabe addresses a Space with `(space, path)` and constructs
    /// an [`lait::control::OrbitAddress`] at the client boundary. Display
    /// assignment is the one daemon request whose wire shape carries only the
    /// local Orbit id, so forwarding the Space id made every real assignment
    /// fail as an invalid Orbit. Keep accepting an already-resolved id for
    /// non-UI callers, and refuse rather than guess when this identity holds
    /// more than one local Orbit in the same Space.
    async fn display_orbit(&self, selector: &str) -> ClientResult<String> {
        if selector.starts_with("orb_") {
            return Ok(selector.to_owned());
        }
        let space = mechanics::ids::SpaceId::parse(selector).ok_or_else(|| {
            ClientError::invalid(format!(
                "'{selector}' is neither a Space id nor a local Orbit id"
            ))
        })?;
        let context = self.host_context().await?;
        let mut matching = context
            .orbits
            .iter()
            .filter(|orbit| orbit.space == selector);
        let orbit = matching.next().ok_or_else(|| {
            ClientError::refused(format!(
                "Space {selector} has no local Orbit registered to this identity"
            ))
        })?;
        if matching.next().is_some() {
            return Err(ClientError::refused(format!(
                "Space {selector} has more than one local Orbit; choose an exact Orbit"
            )));
        }
        Ok(
            lait::control::OrbitAddress::for_store(std::path::Path::new(&orbit.path), space)
                .orbit
                .to_string(),
        )
    }

    /// Render one surface for this machine's own screen.
    ///
    /// Unlike [`Client::display_assignment_put`] this commits nothing, so there
    /// is no handle to return and nothing to revoke afterwards. The Orbit
    /// selector is resolved the same way, because the daemon's display requests
    /// take a local Orbit id and a Space id is the mistake that makes every one
    /// of them fail.
    pub async fn display_present(
        &self,
        selection: &crate::model::PresentationSelection,
    ) -> ClientResult<DisplayPresentationView> {
        let input: serde_json::Value = if selection.input.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&selection.input).map_err(|error| {
                ClientError::invalid(format!("display input is not JSON: {error}"))
            })?
        };
        let orbit = self.display_orbit(&selection.orbit).await?;
        match self
            .display_request(Request::DisplayPresent {
                orbit,
                world: selection.world.clone(),
                surface: selection.surface.clone(),
                input,
                theme: DisplayThemeSetting::Dark,
                width: 1920,
                height: 1080,
                scale_milli: 1000,
                locale: "en".into(),
            })
            .await?
        {
            Response::DisplayPresentation(view) => Ok(*view),
            other => Err(ClientError::internal(format!(
                "unexpected display presentation reply: {other:?}"
            ))),
        }
    }

    /// Add a passphrase as a second way into the coordinator's identifier key.
    ///
    /// The passphrase crosses to the daemon and is never held here: it wraps a
    /// data-encryption key on the other side and is forgotten. Nothing in this
    /// client keeps a copy to be helpful with later.
    pub async fn display_identifier_admit_passphrase(
        &self,
        passphrase: String,
    ) -> ClientResult<()> {
        if passphrase.trim().is_empty() {
            return Err(ClientError::invalid(
                "a passphrase is required to add an unlock path",
            ));
        }
        self.display_ok(Request::DisplayIdentifierAdmitPassphrase { passphrase })
            .await
    }

    pub async fn display_assignment_revoke(&self, assignment: String) -> ClientResult<()> {
        self.display_ok(Request::DisplayAssignmentRevoke { assignment })
            .await
    }

    pub async fn display_device_revoke(&self, device: String) -> ClientResult<()> {
        self.display_ok(Request::DisplayDeviceRevoke { device })
            .await
    }

    async fn display_ok(&self, request: Request) -> ClientResult<()> {
        match self.display_request(request).await? {
            Response::Ok { .. } => Ok(()),
            other => Err(ClientError::internal(format!(
                "unexpected display mutation reply: {other:?}"
            ))),
        }
    }

    async fn display_request(&self, request: Request) -> ClientResult<Response> {
        let reply = self
            .daemon()?
            .request(ControlRoute::Daemon, &request, None)
            .await
            .map_err(|error| {
                ClientError::unreachable(format!("reach display coordinator: {error:#}"))
            })?;
        match reply {
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            response => Ok(response),
        }
    }
}
