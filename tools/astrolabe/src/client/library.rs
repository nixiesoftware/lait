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
    /// The World's id — the key Live-plane scopes name, so presence in a
    /// World can be joined to its row. Empty for the Space front row, which
    /// stands for whatever the Space serves rather than one World.
    pub world: String,
    /// What to call this row. `None` means nothing authoritative names it —
    /// drawn as unnamed rather than as a path or an id dressed up as a name.
    pub display_name: Option<String>,
    /// Where `Open` lands, and when nowhere, why.
    pub opens: Opens,
    pub placement: Placement,
    /// What this World says about how it should be drawn.
    ///
    /// Declared by the World, carried verbatim, and never invented here. A row
    /// for a World this build does not host carries an empty template, which is
    /// the honest answer: nothing has said anything about it.
    pub template: Template,
}

/// A World's own presentation template.
///
/// Everything in it is declared statically by the World and resolved here —
/// there is no asset to fetch and no call to make, which is what keeps listing
/// as cheap as reading a registry. A client draws the slots that are filled and
/// leaves the rest alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Template {
    /// One line saying what this World is for.
    pub tagline: Option<String>,
    /// The colour it is drawn from, packed `0xRRGGBB`. A seed a client derives
    /// a plate or an accent from, locally.
    pub accent: Option<u32>,
    /// The reviewed implementation version bundled for this World.
    pub version: Option<u32>,
    /// The running Orbit's own sync diagnosis. Absent when it was not asked.
    pub sync: Option<SyncStatus>,
    /// Named places inside it, with `{space}` already resolved for this Orbit —
    /// resolved here rather than on a surface, because the substitution is a
    /// fact about this row and not a decision about how to draw it.
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    pub state: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub label: String,
    pub path: String,
}

/// Where `Open` sends the browser for one row — the distinction between the two
/// kinds of row this list holds.
///
/// A **World** row opens at the path that World declared, and at nothing else:
/// `/` is not a guess to make on a World's behalf, and this is the case the
/// original rule was written for.
///
/// A **Space** row is not that case, and treating it as one is what made every
/// row on a freshly started daemon unopenable. A Space row's destination is not
/// a claim about any World: it is the Orbit's own front door — what `lait
/// --orbit <sel>` serves — and what a person finds there is whatever placement
/// activates. Listing stays passive; the click is what places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opens {
    /// A World, at the entry path it declared.
    Declared(String),
    /// The Orbit itself, at its head's root.
    Front,
    /// Nowhere: the Orbit serves this World and *this build* hosts no head for
    /// it. A real state, and a different one from the next — the row exists
    /// because the Orbit really does serve it.
    Unhosted,
    /// Nowhere: this build hosts the World and it declares no entry path.
    Undeclared,
}

impl Opens {
    /// The path `Open` lands on, or `None` when the row cannot be opened.
    pub fn entry_path(&self) -> Option<&str> {
        match self {
            Self::Declared(path) => Some(path),
            // Not a default standing in for a missing declaration — the root
            // *is* the address of an Orbit's head. The two are spelled
            // differently here so that nothing downstream can confuse them.
            Self::Front => Some("/"),
            Self::Unhosted | Self::Undeclared => None,
        }
    }
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
                return Err(ClientError::unreachable(format!("{error:#}")));
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
        let hosted = lait::composition::bundled_packages();

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
                .request_if_running(route.clone(), &Request::WorldsActive)
                .await
            {
                Ok(Response::Worlds { worlds }) => (worlds, Placement::Placed),
                Ok(_) => {
                    // Daemons from before passive World catalog probes were
                    // admitted answer an Error even when this Orbit is already
                    // placed. Status has been a passive probe throughout the
                    // compatibility window, so use it only to recover the
                    // lifecycle fact. We still leave the World list empty
                    // rather than waking or asking the Orbit actively.
                    match daemon
                        .request_if_running(route.clone(), &Request::Status)
                        .await
                    {
                        Ok(Response::Status(_)) => (Vec::new(), Placement::Placed),
                        // Not up. A real answer, and not an error — but it
                        // means the activation record cannot be read, so
                        // nothing is listed for it rather than a guess.
                        Ok(_) => (Vec::new(), Placement::Vacant),
                        Err(_) => (Vec::new(), Placement::Unknown),
                    }
                }
                Err(_) => (Vec::new(), Placement::Unknown),
            };

            let sync = if placement == Placement::Placed {
                match daemon
                    .request_if_running(
                        route,
                        &Request::Diagnose {
                            expected_space: Some(orbit.space.clone()),
                        },
                    )
                    .await
                {
                    Ok(Response::Diagnosis(view)) => view
                        .gates
                        .iter()
                        .find(|gate| gate.id == "synced")
                        .map(|gate| SyncStatus {
                            state: format!("{:?}", gate.state).to_ascii_lowercase(),
                            detail: gate.detail.clone(),
                        }),
                    _ => None,
                }
            } else {
                None
            };

            if activated.is_empty() {
                // The Orbit is real and openable; nothing can be said about its
                // Worlds right now, because a vacant Orbit has no activation
                // record to read. That is precisely the row `Open` is for —
                // opening places the Orbit, and what it activates is then
                // whatever the Space actually serves.
                entries.push(LibraryEntry {
                    orbit: orbit.space.clone(),
                    space: orbit.space.clone(),
                    world_mount: String::new(),
                    world: String::new(),
                    display_name: (!orbit.name.trim().is_empty()).then(|| orbit.name.clone()),
                    opens: Opens::Front,
                    template: Template {
                        sync,
                        ..Template::default()
                    },
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
                    world: world.clone(),
                    display_name: package
                        .map(|p| p.display().name().to_owned())
                        .or_else(|| (!orbit.name.trim().is_empty()).then(|| orbit.name.clone())),
                    // Three outcomes, and the two failures are different facts:
                    // a World this build cannot host at all, and one it hosts
                    // that has not said where to land.
                    opens: package.map_or(Opens::Unhosted, |p| {
                        p.display()
                            .entry_path()
                            .map_or(Opens::Undeclared, |path| Opens::Declared(path.to_owned()))
                    }),
                    template: package.map_or_else(
                        || Template {
                            sync: sync.clone(),
                            ..Template::default()
                        },
                        |p| Template {
                            tagline: p.display().tagline().map(str::to_owned),
                            accent: p.display().accent(),
                            version: hosted.reviewed_state(p.world()).map(|(_, version)| version),
                            sync: sync.clone(),
                            routes: p
                                .display()
                                .routes()
                                .iter()
                                .map(|route| Route {
                                    label: route.label().to_owned(),
                                    path: route.resolve(&orbit.space),
                                })
                                .collect(),
                        },
                    ),
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
            world: String::new(),
            display_name: None,
            opens: Opens::Undeclared,
            template: Template::default(),
            placement: Placement::Vacant,
        };
        assert!(entry.display_name.is_none());
        assert!(
            entry.opens.entry_path().is_none(),
            "an entry path was guessed for a World that declares none"
        );
    }

    /// The distinction the Library exists to make, and the one it was missing:
    /// a World is opened where the World said, and a Space is opened at its
    /// own front door.
    ///
    /// Folding them together is not a cosmetic bug. It made every row on a
    /// freshly started daemon unopenable — a vacant Orbit lists no Worlds, so
    /// every row was a Space row, and every Space row was treated as a World
    /// that had failed to declare an entry path.
    #[test]
    fn a_space_opens_at_its_own_front_door_and_a_world_only_where_it_said() {
        assert_eq!(
            Opens::Front.entry_path(),
            Some("/"),
            "an Orbit with nothing activated cannot be opened, so nothing can \
             ever place it"
        );
        assert_eq!(
            Opens::Declared("/issues".into()).entry_path(),
            Some("/issues")
        );
        assert_eq!(
            Opens::Undeclared.entry_path(),
            None,
            "`/` was guessed on a World's behalf"
        );
        assert_eq!(
            Opens::Unhosted.entry_path(),
            None,
            "a World this build hosts no head for was offered as openable"
        );
        assert_ne!(
            Opens::Unhosted,
            Opens::Undeclared,
            "two different reasons a row cannot be opened collapsed into one"
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
