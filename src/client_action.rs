//! Product-neutral CLI dispatch intent.
//!
//! Parsing a command should determine its orbital destination. The application
//! runner may still carry the historical typed [`Request`] while bundled
//! product adapters are being extracted, but it must not rediscover whether
//! that request belongs to the daemon, a Space, or a World after parsing.

use crate::{
    control::{ControlRoute, Request},
    daemon::OrbitAddress,
};

/// The terminal orbital boundary selected by a client command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientTarget {
    /// The identity-scoped Lait daemon itself.
    Daemon,
    /// Space/Station/Mechanics/lifecycle work reached through one local Orbit.
    Space,
    /// One semantic World reached through one local Orbit.
    World { world: String },
}

/// A parsed command plus its already-determined orbital destination.
///
/// `request` is the temporary compatibility payload shared by today's CLI,
/// MCP, viewer, and v3 per-Orbit daemon. Product command packages can replace
/// it with opaque `WorldCall`s without changing [`ClientTarget`] or the shell's
/// navigation model.
#[derive(Debug, Clone)]
pub struct ClientAction {
    target: ClientTarget,
    request: Request,
}

impl ClientAction {
    /// Adapt the historical typed request surface at the command-registry edge.
    ///
    /// This is deliberately the only compatibility classifier used by the CLI.
    /// New product command registries should construct an explicit World action
    /// instead of adding another post-parse request classifier.
    pub fn from_legacy(request: Request) -> Self {
        let target = if matches!(&request, Request::Stop) {
            ClientTarget::Daemon
        } else if let Some(world) = crate::world::request_world(&request) {
            ClientTarget::World {
                world: world.as_str().to_string(),
            }
        } else {
            ClientTarget::Space
        };
        Self { target, request }
    }

    pub fn target(&self) -> &ClientTarget {
        &self.target
    }

    pub fn request(&self) -> &Request {
        &self.request
    }

    pub fn into_request(self) -> Request {
        self.request
    }

    /// Materialize the complete wire route after the shell resolves an Orbit.
    pub fn route(&self, address: OrbitAddress) -> ControlRoute {
        match &self.target {
            ClientTarget::Daemon => ControlRoute::Daemon,
            ClientTarget::Space => ControlRoute::Space { address },
            ClientTarget::World { world } => ControlRoute::World {
                address,
                world: world.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::ids::SpaceId;
    use std::path::Path;

    #[test]
    fn compatibility_requests_become_explicit_orbital_actions() {
        let issue = ClientAction::from_legacy(Request::IssueView {
            reff: "ENG-1".into(),
        });
        assert!(matches!(
            issue.target(),
            ClientTarget::World { world } if world == "com.lait.issues"
        ));

        let station = ClientAction::from_legacy(Request::Who);
        assert_eq!(station.target(), &ClientTarget::Space);

        let shutdown = ClientAction::from_legacy(Request::Stop);
        assert_eq!(shutdown.target(), &ClientTarget::Daemon);
    }

    #[test]
    fn an_action_materializes_the_route_it_selected_at_parse_time() {
        let address =
            OrbitAddress::for_store(Path::new("/tmp/lait-action"), SpaceId::from_digest([7; 16]));
        let action = ClientAction::from_legacy(Request::IssueView {
            reff: "ENG-1".into(),
        });
        assert!(matches!(
            action.route(address),
            ControlRoute::World { world, .. } if world == "com.lait.issues"
        ));
    }
}
