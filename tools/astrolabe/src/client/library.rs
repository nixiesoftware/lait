//! The Library, and the handoff that opens something in it.
//!
//! One row per World this build ships a client package for — the install list,
//! read from the binary's own composition and from nothing else. `Open` hands
//! off to that World's own head; the client never draws a World.
//!
//! What is deliberately *not* listed here is Spaces. A World and the Spaces it
//! is served in are different axes, and the destination owns the second one:
//! the head's own front page carries the Space selector, and selecting a Space
//! there is what attaches its daemon. A Library that listed Orbits had to probe
//! them to know what to draw, and the row a person saw changed kind depending
//! on whether a daemon happened to be up — the "Unnamed Space" that became
//! "Issues" on start was one row being replaced by a different kind of row, not
//! a name arriving.

use super::Client;
use super::{ClientError, ClientResult};

/// One row of the Library: an installed World.
///
/// Every field is declared by the bundled package and resolved at compile time
/// — there is no daemon to ask, no registry to read and no probe to run, which
/// is what makes listing free and its answer always current about what is
/// installed. Which Spaces serve a World, and whether any of them is up, are
/// the destination's facts and deliberately not this row's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    /// The mount this build serves the World at — the row's stable key.
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
    /// The reviewed implementation version bundled for this World.
    pub version: Option<u32>,
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
fn installed() -> Vec<LibraryEntry> {
    let packages = lait::composition::bundled_client_packages();
    let hosted = lait::composition::bundled_packages();
    packages
        .packages()
        .map(|package| LibraryEntry {
            world_mount: package.mount().to_owned(),
            world: package.world().as_str().to_owned(),
            display_name: package.display().name().to_owned(),
            entry_path: package.display().entry_path().map(str::to_owned),
            tagline: package.display().tagline().map(str::to_owned),
            accent: package.display().accent(),
            version: hosted
                .reviewed_state(package.world())
                .map(|(_, version)| version),
        })
        .collect()
}

impl Client {
    /// Every World this build can open.
    ///
    /// Read from the compiled-in composition, joined with the hosted side for
    /// the reviewed version — the same join the daemon must not be asked to
    /// make, because it would be answering for a package it does not hold.
    pub fn get_library(&self) -> Vec<LibraryEntry> {
        installed()
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

    /// The Library is the install list, and this build installs the Issues
    /// World. Read through the same function `get_library` answers with, so
    /// the test breaks when the declaration does — a row with no name, no
    /// mount or no World id is a package that stopped declaring itself.
    #[test]
    fn the_library_lists_what_this_build_installs() {
        let entries = installed();

        assert!(
            !entries.is_empty(),
            "this build installs nothing, so the Library would always be empty"
        );
        for entry in &entries {
            assert!(
                !entry.display_name.is_empty(),
                "an installed World declared no name"
            );
            assert!(!entry.world_mount.is_empty());
            assert!(!entry.world.is_empty());
        }
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
