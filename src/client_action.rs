//! Product-neutral CLI dispatch intent.
//!
//! Parsing a command should determine its orbital destination. The application
//! runner may still carry the historical typed [`Request`] while bundled
//! product adapters are being extracted, but it must not rediscover whether
//! that request belongs to the daemon, a Space, or a World after parsing.

use crate::{
    control::{ControlRoute, Request},
    daemon::OrbitAddress,
    orbital::WorldCall,
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
/// Space/daemon compatibility calls remain typed while product packages emit
/// opaque [`WorldCall`]s directly. The payload therefore fixes both destination
/// and wire shape at parse time; transport never reclassifies product intent.
#[derive(Debug, Clone)]
pub struct ClientAction {
    target: ClientTarget,
    payload: ClientPayload,
}

#[derive(Debug, Clone)]
pub enum ClientPayload {
    /// Space/daemon compatibility surface, pending its own typed split.
    Control(Request),
    /// A product-owned application call.
    World(WorldCall),
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
        Self {
            target,
            payload: ClientPayload::Control(request),
        }
    }

    /// Construct a product action directly from its package-owned call.
    pub fn world(call: WorldCall) -> Self {
        Self {
            target: ClientTarget::World {
                world: call.world().as_str().to_string(),
            },
            payload: ClientPayload::World(call),
        }
    }

    pub fn target(&self) -> &ClientTarget {
        &self.target
    }

    pub fn payload(&self) -> &ClientPayload {
        &self.payload
    }

    pub fn request(&self) -> Option<&Request> {
        match &self.payload {
            ClientPayload::Control(request) => Some(request),
            ClientPayload::World(_) => None,
        }
    }

    pub fn into_request(self) -> Option<Request> {
        match self.payload {
            ClientPayload::Control(request) => Some(request),
            ClientPayload::World(_) => None,
        }
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

    #[test]
    fn a_world_call_never_needs_request_classification() {
        let call = WorldCall::new(
            crate::world::contract::world_id(),
            "issues.control",
            1,
            br#"{"cmd":"project_list"}"#.to_vec(),
        )
        .unwrap();
        let action = ClientAction::world(call);
        assert!(matches!(
            action.target(),
            ClientTarget::World { world } if world == "com.lait.issues"
        ));
        assert!(matches!(action.payload(), ClientPayload::World(_)));
        assert!(action.request().is_none());
    }
}
