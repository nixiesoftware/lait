//! The Space plane.
//!
//! A Space sits *below* a World and may carry several, so its administration
//! belongs to the client rather than to any one World's settings page. A World
//! drawing the membership of the Space it happens to sit in is the layering
//! this module reverses.

use lait::control::{ControlRoute, Request, Response};

use super::{Client, ClientError, ClientResult};

impl Client {
    /// Ask a Space about this actor's standing in it.
    ///
    /// Routed per Orbit rather than per identity: one identity may hold very
    /// different standing in two Spaces, and a single answer for "who am I"
    /// would have to pick one and be wrong about the other.
    pub async fn whoami(&self, store: &std::path::Path, space: &str) -> ClientResult<Standing> {
        let Some(parsed) = mechanics::ids::SpaceId::parse(space) else {
            return Err(ClientError::invalid(format!("'{space}' is not a Space id")));
        };
        let route = ControlRoute::Orbit {
            address: lait::control::OrbitAddress::for_store(store, parsed),
        };
        // `request_if_running` keeps this passive: asking who I am must not
        // place an Orbit that was not up.
        let reply = self
            .daemon()?
            .request_if_running(route, &Request::Whoami)
            .await
            .map_err(|error| ClientError::unreachable(format!("read standing: {error:#}")))?;
        match reply {
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Ok(Standing {
                described: format!("{other:?}"),
            }),
        }
    }
}

/// What a Space says about this actor.
///
/// Deliberately thin for now: membership, roles and custody are CLIENT-16, and
/// stubbing a rich shape here would invite a surface to draw fields nothing
/// fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub described: String,
}
