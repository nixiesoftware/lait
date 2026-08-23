//! The Library, and the handoff that opens something in it.
//!
//! One row per selected immutable World release in the identity's installation.
//! The manifest is read passively; listing launches no runner. `Open` hands off
//! to that World's own head; the client never draws a World.
//!
//! What is deliberately *not* listed here is Spaces. A World and the Spaces it
//! is served in are different axes, and the destination owns the second one:
//! the head's own front page carries the Space selector, and selecting a Space
//! there is what attaches its daemon. A Library that listed Orbits had to probe
//! them to know what to draw, and the row a person saw changed kind depending
//! on whether a daemon happened to be up — the "Unnamed Space" that became
//! "Issues" on start was one row being replaced by a different kind of row, not
//! a name arriving.

use std::collections::BTreeMap;
use std::path::Path;

use super::Client;
use super::{ClientError, ClientResult};

/// What this machine last learned about one World's channel.
///
/// Read from disk rather than asked for over a plane, for the same reason the
/// product's own standing is: the fact outlives both processes, so the client
/// and the daemon need not be alive at the same moment for it to be true.
///
/// Absent for a World nothing has ever checked — and absence is not "up to
/// date". A Library row with no standing draws exactly what it drew before any
/// of this existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldStanding {
    /// The selected bundle version. `None` means no valid release is selected.
    pub serving: Option<String>,
    /// The version the World's channel named when it was last asked.
    pub available: Option<String>,
    /// The channel is known to hold a bundle this machine is not serving, and
    /// this build can run it. Every uncertainty answers false — see
    /// `lait::update::world::Standing::behind`.
    pub behind: bool,
    /// A newer bundle exists that this build cannot run, each unmet
    /// requirement named. Not actionable, and deliberately not `behind`.
    pub unmet: Option<Vec<String>>,
    /// The durable native update operation, when consent has been recorded.
    pub operation: Option<String>,
    pub phase: Option<String>,
    pub progress: Option<String>,
    pub message: Option<String>,
}

/// What the daemon has learned about each installed World.
///
/// Keyed by World id. Empty when there is no identity bound yet, which is the
/// same answer as "nothing has been checked" and draws the same way.
///
/// Enumerates the Worlds itself rather than taking the Library's rows, because
/// this is sampled on the host tick — once a second, beside the two control
/// round trips already there — and a signature that needed the rows would have
/// made the cheap half depend on the expensive one. Two small file reads per
/// World; the Library is a handful of rows and never a corpus.
pub fn world_standings(identity: Option<&Path>) -> BTreeMap<String, WorldStanding> {
    let Some(identity) = identity else {
        return BTreeMap::new();
    };
    let worlds = lait::serve::head::worlds_root(identity);
    lait::world::installed::declarations(&worlds)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|declaration| {
            let world = declaration.manifest.id;
            let standing = lait::update::world::standing(&worlds, &world)?;
            let upgrade = lait::update::consent::load(&worlds, &world).ok().flatten();
            let progress = upgrade.as_ref().map(|job| {
                if let Some(remaining) = job.remaining_records {
                    format!(
                        "{} records completed, {remaining} remaining",
                        job.completed_records
                    )
                } else {
                    format!(
                        "{} of {} Spaces completed",
                        job.completed_spaces, job.total_spaces
                    )
                }
            });
            Some((
                world,
                WorldStanding {
                    behind: standing.behind(),
                    serving: standing.serving,
                    available: standing.channel,
                    unmet: standing.unmet,
                    operation: upgrade.as_ref().map(|job| job.operation_hex()),
                    phase: upgrade.as_ref().map(|job| job.phase.as_str().to_owned()),
                    progress,
                    message: upgrade.and_then(|job| job.message),
                },
            ))
        })
        .collect()
}

/// One row of the Library: an installed World.
///
/// Every field is read from the selected release's signed `world.json`. There
/// is no daemon to ask and no runner to launch, so listing stays passive and
/// current about what is installed. Which Spaces serve a World, and whether
/// any of them is up, are the destination's facts and deliberately not this
/// row's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    /// The mount this release serves the World at — the row's stable key.
    pub world_mount: String,
    /// The World's id — the key Live-plane scopes name, so presence in a
    /// World can be joined to its row.
    pub world: String,
    /// What the World calls itself. Always present: an installed package
    /// declares its name, so there is no unnamed row to draw.
    pub display_name: String,
    /// The entry path the World declared, or `None` when it declares none.
    /// `/` is not a guess to make on a World's behalf.
    pub entry_path: Option<String>,
    /// One line saying what this World is for.
    pub tagline: Option<String>,
    /// The colour it is drawn from, packed `0xRRGGBB`. A seed a client
    /// derives a plate or an accent from, locally.
    pub accent: Option<u32>,
    /// The reviewed implementation version declared by this World release.
    pub version: Option<u32>,
}

/// A World's own artwork, read from its selected immutable release.
///
/// Deliberately *not* a field on [`LibraryEntry`]. The Library entry is part of
/// the view, and the view is pushed whole to every attached surface on every
/// pump — a mark and a hero riding in it would be re-marshalled on every
/// presence sample. This is read once per World instead, by a surface that has
/// decided to draw it.
///
/// Named apart from the interface type it feeds (`api::WorldArtwork`) because
/// the bridge codegen keys types by bare name across the whole crate: two
/// `WorldArtwork`s and it warns that it picked one at random. They are the
/// same shape today, which is exactly why the day one of them changes would be
/// the confusing one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Artwork {
    /// PNG bytes for the square mark, or `None` where the World ships none —
    /// which is a World to be drawn from its accent, not a missing file.
    pub mark: Option<Vec<u8>>,
    /// PNG bytes for the square hero frame, under the same rule.
    pub hero: Option<Vec<u8>>,
}

/// The artwork one installed World declares, by mount.
///
/// An unknown mount answers with no artwork rather than an error: a surface
/// asking about a World this build does not install is asking a question whose
/// honest answer is "nothing to draw".
pub fn artwork(mount: &str) -> Artwork {
    declarations(None)
        .into_iter()
        .find(|declaration| declaration.manifest.mount() == mount)
        .map(|declaration| Artwork {
            mark: declaration
                .manifest
                .mark
                .as_ref()
                .and_then(|path| std::fs::read(declaration.root.join(path)).ok()),
            hero: declaration
                .manifest
                .hero
                .as_ref()
                .and_then(|path| std::fs::read(declaration.root.join(path)).ok()),
        })
        .unwrap_or_default()
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

/// The install list itself, free of [`Client`] so a test can read it without
/// constructing one. What `get_library` returns IS this — delegation, not a
/// second copy that could drift.
///
/// `pub(crate)` because the runtime also reads it on the one path that has no
/// client to ask: a supervisor that failed to start can still read signed
/// manifests without starting their processes.
pub(crate) fn installed() -> Vec<LibraryEntry> {
    installed_for(None)
}

fn declarations(identity: Option<&Path>) -> Vec<lait::world::installed::Declaration> {
    let identity = identity
        .map(std::path::PathBuf::from)
        .or_else(|| lait::config::Selection::default().identity_dir().ok());
    identity
        .and_then(|identity| {
            lait::world::installed::declarations(&lait::serve::head::worlds_root(&identity)).ok()
        })
        .unwrap_or_default()
}

fn installed_for(identity: Option<&Path>) -> Vec<LibraryEntry> {
    declarations(identity)
        .into_iter()
        .map(|declaration| library_entry(declaration.manifest))
        .collect()
}

fn library_entry(manifest: world_interface::manifest::WorldManifest) -> LibraryEntry {
    let entry_path = manifest.launch.iter().find_map(|entry| {
        (entry.present == world_interface::manifest::Present::Primary
            && entry
                .when
                .as_ref()
                .is_none_or(|when| when.admits(std::env::consts::OS, std::env::consts::ARCH)))
        .then(|| match &entry.target {
            world_interface::manifest::Target::Web { path } => Some(path.clone()),
            _ => None,
        })
        .flatten()
    });
    LibraryEntry {
        world_mount: manifest.mount().to_owned(),
        world: manifest.id.clone(),
        display_name: manifest.display_name().to_owned(),
        entry_path,
        tagline: manifest.tagline,
        accent: manifest.accent,
        version: manifest.implementation_version,
    }
}

fn world_id_for_mount_in(entries: &[LibraryEntry], mount: &str) -> Option<String> {
    entries
        .iter()
        .find(|entry| entry.world_mount == mount)
        .map(|entry| entry.world.clone())
}

/// The World id behind one mount, when this build installs it.
///
/// The mount and the id are deliberately different strings for different jobs:
/// the mount is the stable key a surface and a URL carry, the id is the
/// reverse-domain name the daemon scopes update consent by. A surface holds
/// only the mount, so the selected manifest is where one becomes the other.
/// An unknown mount answers with nothing rather than letting a mount travel
/// onward dressed as an id.
pub fn world_id_for_mount(mount: &str) -> Option<String> {
    world_id_for_mount_in(&installed(), mount)
}

impl Client {
    pub(crate) fn installed_world_ids(&self) -> Vec<String> {
        installed_for(self.inner.identity.as_deref())
            .into_iter()
            .map(|entry| entry.world)
            .collect()
    }

    /// Every selected World release this identity can open, read passively
    /// from its signed declaration.
    pub fn get_library(&self) -> Vec<LibraryEntry> {
        installed_for(self.identity_dir().as_deref())
    }

    /// The URL `Open` sends the browser to, given a ticket the head minted.
    ///
    /// Minting is deliberately not here. A ticket must be *spent* by the
    /// process that will be presented with it, so the store lives in the head
    /// (`POST /api/launch`) and a client that minted its own would be issuing
    /// credentials against a store nothing checks. This composes the URL and
    /// nothing else.
    pub fn launch_url(
        head: &str,
        entry_path: &str,
        ticket: &str,
        expires_at_ms: u64,
    ) -> ClientResult<LaunchTicket> {
        if ticket.trim().is_empty() {
            return Err(ClientError::internal(
                "a launch was composed with no credential in it",
            ));
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

    fn selected_manifest() -> world_interface::manifest::WorldManifest {
        world_interface::manifest::WorldManifest::parse(
            br#"{
                "format": 1,
                "id": "com.lait.issues",
                "version": "1.2.3",
                "mount": "issues",
                "name": "Issues",
                "tagline": "Track the work",
                "implementation_version": 4,
                "runners": [{"preferred": true, "program": "bin/world"}],
                "launch": [{
                    "id": "app",
                    "present": "primary",
                    "target": {"type": "web", "path": "/"}
                }]
            }"#,
        )
        .expect("a selected World declaration")
    }

    /// The Library is derived from selected signed declarations, not from what
    /// happened to be installed in the developer's ambient identity.
    #[test]
    fn the_library_maps_the_selected_declaration() {
        let entry = library_entry(selected_manifest());
        assert_eq!(entry.display_name, "Issues");
        assert_eq!(entry.world_mount, "issues");
        assert_eq!(entry.world, "com.lait.issues");
        assert_eq!(entry.entry_path.as_deref(), Some("/"));
        assert_eq!(entry.version, Some(4));
    }

    /// Update consent on the daemon is scoped by World id, but a surface holds
    /// only the mount. The resolver is the seam between the two names: every
    /// installed mount answers with the id its own row carries, and an unknown
    /// mount answers with nothing rather than travelling onward as an id.
    #[test]
    fn a_mount_resolves_to_the_world_id_updates_are_scoped_by() {
        let entries = vec![library_entry(selected_manifest())];
        assert_eq!(
            world_id_for_mount_in(&entries, "issues").as_deref(),
            Some("com.lait.issues")
        );
        assert_eq!(world_id_for_mount_in(&entries, "no-such-mount"), None);
    }

    /// A World that declares no entry path stays unopenable rather than
    /// being opened at `/` on a guess. The declaration is the World's own
    /// statement about itself; absence crosses as absence.
    #[test]
    fn an_undeclared_entry_path_is_carried_as_absent_not_guessed() {
        let entry = LibraryEntry {
            world_mount: "issues".into(),
            world: "com.lait.issues".into(),
            display_name: "Issues".into(),
            entry_path: None,
            tagline: None,
            accent: None,
            version: None,
        };
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
}
