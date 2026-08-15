//! Finding `lait.exe`, and refusing the wrong one.
//!
//! The pair ships together and is installed together. Two rules follow, and
//! both are rules rather than conventions because the failure they prevent is
//! silent:
//!
//! 1. **The sidecar is fixed.** It is resolved relative to the running
//!    executable, never chosen by the person and never read from user input. A
//!    configurable path here is an arbitrary-executable problem wearing a
//!    settings field — the client spawns what it finds, so what it finds must
//!    not be attacker-influenced.
//! 2. **A mismatch is reported as a mismatch.** Attaching to an incompatible
//!    daemon and discovering it later through a decode failure is exactly the
//!    outcome this exists to prevent: the symptom appears far from the cause,
//!    wearing the costume of a protocol bug.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The version of `lait` this build of Astrolabe was compiled against.
///
/// Read from the linked crate rather than written down here, because a
/// hand-maintained figure is one refactor away from claiming compatibility with
/// something nobody built against. `lait::VERSION` is already the single
/// constant its own `--version` and host-plane orientation both answer with, so
/// three places cannot disagree about which build this is.
pub const EXPECTED: &str = lait::VERSION;

/// Where the sidecar is, given where we are.
///
/// Beside the running executable. Not on `PATH`, which would let whatever a
/// person installed last decide; not from configuration, which would make it
/// user input; not from an environment variable, which is user input a parent
/// process can set.
/// Where this client keeps what it manages.
///
/// Under the user's local data directory, not beside the executable: a program
/// directory may be read-only, may be shared between users, and is replaced
/// wholesale by an upgrade.
///
/// It lives beside the sidecar's own resolution because they are the same kind
/// of question — *where does this installation keep its things* — and because
/// the interface must not be the one answering it. A path computed on the far
/// side of the bridge would be a second opinion about where the client's state
/// lives, and the two would differ on exactly the machine where it mattered.
pub fn state_root() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context("locate a local data directory for the managed state root")?;
    let root = base.join("Astrolabe").join("devices");
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create the managed state root {}", root.display()))?;
    Ok(root)
}

pub fn resolve() -> Result<PathBuf> {
    let current = std::env::current_exe().context("locate the running executable")?;
    Ok(beside(&current))
}

/// [`beside`], reachable from the paired layout test.
///
/// The pairing is asserted across two crates — this half and `lait`'s
/// `update::custody_of` — because one layout described in two places drifts
/// silently otherwise, and the failure only shows on an installed machine.
pub fn beside_for_test(executable: &Path) -> PathBuf {
    beside(executable)
}

fn beside(executable: &Path) -> PathBuf {
    let name = if cfg!(windows) { "lait.exe" } else { "lait" };
    executable.with_file_name(name)
}

/// Whether a daemon reporting `found` is one this build will attach to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    /// Say both figures. "Incompatible version" without the two numbers sends
    /// somebody to read a changelog to learn what they already had on screen.
    Mismatch {
        expected: String,
        found: String,
    },
    /// The daemon answered something that is not a version at all.
    Unreadable {
        found: String,
    },
}

/// Compare a reported version against what this build expects.
///
/// The rule is *same major and minor*. Patch releases of a daemon are the ones
/// that carry fixes a client should pick up without being rebuilt, and holding
/// out for an exact match would make every patch a coordinated release. A minor
/// bump is where the control protocol is allowed to move, so it is where this
/// stops.
pub fn check(found: &str) -> Compatibility {
    let Some(found_pair) = major_minor(found) else {
        return Compatibility::Unreadable {
            found: found.to_owned(),
        };
    };
    let Some(expected_pair) = major_minor(EXPECTED) else {
        return Compatibility::Unreadable {
            found: found.to_owned(),
        };
    };
    if found_pair == expected_pair {
        Compatibility::Compatible
    } else {
        Compatibility::Mismatch {
            expected: EXPECTED.to_owned(),
            found: found.to_owned(),
        }
    }
}

/// `1.2.3-rc.1+build` → `(1, 2)`.
///
/// Hand-parsed rather than pulling in a semver crate: the whole rule is "the
/// first two numbers", and a dependency to compare two integers would be a
/// dependency to audit, notice and ship.
fn major_minor(version: &str) -> Option<(u64, u64)> {
    let core = version
        .split(['-', '+'])
        .next()
        .unwrap_or(version)
        .trim_start_matches('v');
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidecar_is_resolved_beside_the_running_executable() {
        let installed = Path::new("C:/Program Files/Astrolabe/astrolabe.exe");
        let sidecar = beside(installed);
        assert_eq!(
            sidecar.parent(),
            installed.parent(),
            "the sidecar was looked for somewhere other than beside us"
        );
        assert!(sidecar
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("lait")));
    }

    #[test]
    fn a_patch_release_is_compatible_and_a_minor_bump_is_not() {
        let (major, minor) = major_minor(EXPECTED).expect("this build's version parses");

        assert_eq!(
            check(&format!("{major}.{minor}.999")),
            Compatibility::Compatible,
            "a patch release of the daemon was refused"
        );
        assert!(
            matches!(
                check(&format!("{major}.{}.0", minor + 1)),
                Compatibility::Mismatch { .. }
            ),
            "a minor bump was accepted, so the control protocol may have moved unnoticed"
        );
        assert!(matches!(
            check(&format!("{}.{minor}.0", major + 1)),
            Compatibility::Mismatch { .. }
        ));
    }

    /// The message has to carry both figures, or it sends somebody to a
    /// changelog to learn what was already on screen.
    #[test]
    fn a_mismatch_names_both_versions() {
        let Compatibility::Mismatch { expected, found } = check("999.999.0") else {
            panic!("a wildly different version was accepted");
        };
        assert_eq!(found, "999.999.0");
        assert_eq!(expected, EXPECTED);
    }

    /// Not a version is not a mismatch. Conflating them would report a daemon
    /// that answered garbage as one that answered a number we disagreed with.
    #[test]
    fn something_that_is_not_a_version_is_unreadable_rather_than_mismatched() {
        assert!(matches!(check(""), Compatibility::Unreadable { .. }));
        assert!(matches!(check("dev"), Compatibility::Unreadable { .. }));
        assert!(matches!(check("1"), Compatibility::Unreadable { .. }));
    }

    /// Prerelease and build metadata do not change which daemon a client talks
    /// to; they change which build of it.
    #[test]
    fn prerelease_and_build_metadata_are_ignored() {
        let (major, minor) = major_minor(EXPECTED).expect("version parses");
        assert_eq!(
            check(&format!("{major}.{minor}.0-rc.1+abc123")),
            Compatibility::Compatible
        );
    }
}
