//! Native Astrolabe control of the self-hosted display coordinator.

use lait::control::{
    ControlRoute, DisplayAssignmentSyncSetting, DisplayCoordinatorView, DisplayStaleActionSetting,
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
