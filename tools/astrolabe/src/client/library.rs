//! The Library, and the handoff that opens something in it.
//!
//! One row per openable thing — an Orbit, and a World mounted in it. `Open`
//! hands off to that World's own head; the client never draws a World.

use lait::control::{ControlRoute, HostReply, Request, Response};

use super::{Client, ClientError, ClientResult};

/// One row of the Library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    pub orbit: String,
    pub space: String,
    pub world_mount: String,
    /// What to call this row. `None` means nothing authoritative names it —
    /// drawn as unnamed rather than as a path or an id dressed up as a name.
    pub display_name: Option<String>,
    /// Where `Open` lands. `None` until a World declares one; a row without it
    /// cannot be opened, and says so instead of guessing `/`.
    pub entry_path: Option<String>,
    pub placement: Placement,
}

/// Whether an Orbit is currently up.
///
/// Observed, never caused. The distinction is the whole invariant: a Library
/// that placed every Orbit in order to draw itself would make listing cost what
/// opening costs, and on a machine with many Orbits that is the difference
/// between a front page and a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Up now, and answering.
    Placed,
    /// Not up. Not an error, and not something listing corrects.
    Vacant,
    /// The daemon could not be asked. Distinct from `Vacant`, because
    /// rendering "nobody could ask" as "not running" is the same defect class
    /// as rendering a sampling failure as "no peers".
    Unknown,
}

/// A credential minted for one launch.
///
/// Short-lived, single-use, and scoped to the Orbit being opened. It is never a
/// long-lived process-wide bearer, because a launch URL lands in browser
/// history, in a synchronised profile, and in the shell's recent list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTicket {
    pub url: String,
    pub expires_at_ms: u64,
}

impl Client {
    /// Every Orbit this device serves, and the Worlds mounted in them.
    ///
    /// Passive by construction: this asks the daemon what it already knows and
    /// places nothing. An Orbit that is not up is listed as `Vacant` and is not
    /// woken to improve the row.
    pub async fn get_library(&self) -> ClientResult<Vec<LibraryEntry>> {
        let daemon = self.daemon()?;
        let context = match daemon
            .request(ControlRoute::Daemon, &Request::HostContext, None)
            .await
        {
            Ok(Response::Host(reply)) => reply,
            Ok(Response::Error { message, .. }) => return Err(ClientError::refused(message)),
            Ok(other) => {
                return Err(ClientError::internal(format!(
                    "unexpected host context reply: {other:?}"
                )));
            }
            Err(error) => {
                return Err(ClientError::unreachable(format!(
                    "read host context: {error:#}"
                )));
            }
        };
        let HostReply::Context { orbits, .. } = context else {
            return Err(ClientError::internal(
                "host context reply carried no context",
            ));
        };

        // This build's own declarations, read once. The Space says *which*
        // Worlds it serves; this says how to draw and open one, and joining
        // them here is what keeps the daemon from answering for a package it
        // does not hold.
        let packages = lait::composition::bundled_client_packages();

        let mut entries = Vec::new();
        for orbit in orbits {
            let Some(space) = mechanics::ids::SpaceId::parse(&orbit.space) else {
                continue;
            };
            let route = lait::control::ControlRoute::Orbit {
                address: lait::control::OrbitAddress::for_store(
                    std::path::Path::new(&orbit.path),
                    space,
                ),
            };
            // `request_if_running` is what makes listing passive: a vacant
            // Orbit answers "not running" rather than being placed to produce a
            // row. Placement is what `Open` causes, not what listing costs.
            let (activated, placement) = match daemon
                .request_if_running(route, &Request::WorldsActive)
                .await
            {
                Ok(Response::Worlds { worlds }) => (worlds, Placement::Placed),
                // Not up. A real answer, and not an error — but it means the
                // activation record cannot be read, so nothing is listed for
                // it rather than a guess being listed.
                Ok(_) => (Vec::new(), Placement::Vacant),
                Err(_) => (Vec::new(), Placement::Unknown),
            };

            if activated.is_empty() {
                // The Orbit is real and openable-in-principle; nothing can be
                // said about its Worlds right now. One row for the Space, with
                // no World and no entry path, beats silently dropping it.
                entries.push(LibraryEntry {
                    orbit: orbit.space.clone(),
                    space: orbit.space.clone(),
                    world_mount: String::new(),
                    display_name: (!orbit.name.trim().is_empty()).then(|| orbit.name.clone()),
                    entry_path: None,
                    placement,
                });
                continue;
            }

            for world in activated {
                let package = packages
                    .packages()
                    .find(|package| package.world().as_str() == world);
                entries.push(LibraryEntry {
                    orbit: orbit.space.clone(),
                    space: orbit.space.clone(),
                    // A World this build does not host is still a row: the
                    // Orbit serves something this program cannot open, and
                    // saying so beats pretending it is not there.
                    world_mount: package.map_or_else(|| world.clone(), |p| p.mount().to_owned()),
                    display_name: package
                        .map(|p| p.display().name().to_owned())
                        .or_else(|| (!orbit.name.trim().is_empty()).then(|| orbit.name.clone())),
                    entry_path: package.and_then(|p| p.display().entry_path().map(str::to_owned)),
                    placement,
                });
            }
        }
        Ok(entries)
    }

    /// The URL `Open` sends the browser to, given a ticket the head minted.
    ///
    /// Minting is deliberately not here. A ticket must be *spent* by the
    /// process that will be presented with it, so the store lives in the head
    /// (`POST /api/launch`) and a client that minted its own would be issuing
    /// credentials against a store nothing checks. This composes the URL and
    /// nothing else.
    ///
    /// Nothing here opens a browser either: a library call that launched a
    /// process as a side effect could not be tested without one.
    pub fn launch_url(
        head: &str,
        entry_path: &str,
        ticket: &str,
        expires_at_ms: u64,
    ) -> ClientResult<LaunchTicket> {
        if ticket.trim().is_empty() {
            return Err(ClientError::invalid("a launch needs a ticket"));
        }
        Ok(LaunchTicket {
            url: format!(
                "{}{}?ticket={}",
                head.trim_end_matches('/'),
                if entry_path.starts_with('/') {
                    entry_path
                } else {
                    "/"
                },
                ticket.trim()
            ),
            expires_at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row nobody named is drawn as unnamed. Falling back to a path or an id
    /// would put something in the name column that is not a name, and a person
    /// cannot tell that apart from a World that is genuinely called that.
    #[test]
    fn an_unnamed_row_says_so_rather_than_inventing_a_label() {
        let entry = LibraryEntry {
            orbit: "orb_one".into(),
            space: "ws_one".into(),
            world_mount: "issues".into(),
            display_name: None,
            entry_path: None,
            placement: Placement::Vacant,
        };
        assert!(entry.display_name.is_none());
        assert!(
            entry.entry_path.is_none(),
            "an entry path was guessed for a World that declares none"
        );
    }

    /// The ticket travels in the query, and the entry path the World declared
    /// is where it lands. A launch URL that dropped either would open the head
    /// unauthenticated, at the wrong place.
    #[test]
    fn a_launch_url_carries_the_ticket_to_the_declared_entry() {
        let ticket =
            Client::launch_url("http://127.0.0.1:7717/", "/", "abc123", 42).expect("a launch url");
        assert_eq!(ticket.url, "http://127.0.0.1:7717/?ticket=abc123");
        assert_eq!(ticket.expires_at_ms, 42);

        assert!(
            Client::launch_url("http://127.0.0.1:7717", "/", "  ", 0).is_err(),
            "a launch was composed with no credential in it"
        );
    }

    /// Three states, not two. A surface that folds `Unknown` into `Vacant`
    /// reports a machine nobody could ask as a machine with nothing running.
    #[test]
    fn placement_distinguishes_vacant_from_unknown() {
        assert_ne!(Placement::Vacant, Placement::Unknown);
        assert_ne!(Placement::Placed, Placement::Unknown);
    }
}
