//! Worlds this device is being used to *write*, as opposed to run.
//!
//! # Why these are not installed Worlds with a different source
//!
//! A Library row's claim — this is release 0.9.3, installed, verified — is the
//! only thing this client says about what a person is running, and it is worth
//! exactly as much as the number of ways it can be false. There was briefly a
//! recorded override that let a released World serve a directory somebody was
//! working on, with a promise to draw that fact on every surface. That trade is
//! strictly worse than not allowing it: it holds only while every surface
//! remembers, and it had already stopped holding in two places by the time it
//! was reviewed.
//!
//! [`super::installed::load`] knew this all along. Every declaration it loads
//! carries a 32-byte release digest, so a directory cannot become one without
//! inventing a digest for bytes nobody signed.
//!
//! So a directory being worked on gets an entry of its own. It is not a variant
//! of the released World, it does not borrow its identity, and it never claims
//! a version it was not given.
//!
//! # What one is
//!
//! An **unsealed World tree**: exactly what a World's build produces before
//! anything signs it — a `world.json` beside the runner and pages it declares.
//! It is the same shape a release is, minus the seal, which is what makes this
//! a development path rather than a second kind of World.
//!
//! # What it is never given
//!
//! A digest. [`world_runner::Release`] carries one to the World's own process
//! as `LAIT_WORLD_RELEASE`, so it is a provenance label rather than a gate —
//! nothing re-verifies it at launch, because staging already did. Minting a
//! plausible-looking one here would make a local tree indistinguishable from a
//! release *to the World itself*, which is the one place the distinction has to
//! survive.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use world_interface::manifest::WorldManifest;

/// Where local World registrations live.
///
/// Beside `world-bundles-v1` and never inside it. Everything under that root
/// is an installation, and a thing that is not an installation must not be
/// able to sit in the directory that enumerates them — which is not a
/// hypothetical: one stray directory there used to stop a head and a daemon
/// from starting at all.
pub fn registrations_root(identity: &Path) -> PathBuf {
    identity.join("world-local-v1")
}

/// One registered local World.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// The tree this World is read from. Absolute, and re-proved on read.
    pub dir: PathBuf,
}

/// A registered local World, resolved against the tree it names.
#[derive(Debug, Clone)]
pub struct Local {
    /// This entry's stable key, and the reason it can never be mistaken for an
    /// installed World: it is namespaced by construction. See [`key_for`].
    pub key: String,
    /// The tree it is read from.
    pub dir: PathBuf,
    /// What the tree declares about itself, when it could be read.
    ///
    /// `None` is a registration whose tree is gone or unreadable — which is a
    /// row that must still be drawn, saying so, rather than an entry that
    /// silently stops existing. A registration nobody can see is a
    /// registration nobody can remove.
    pub manifest: Option<WorldManifest>,
}

impl Local {
    /// What to call it in a list. Falls back to the key, which is at least the
    /// thing you would type to remove it.
    pub fn display_name(&self) -> String {
        match &self.manifest {
            Some(manifest) => manifest.name.clone().unwrap_or_else(|| {
                manifest
                    .id
                    .rsplit('.')
                    .next()
                    .unwrap_or(&manifest.id)
                    .into()
            }),
            None => self.key.clone(),
        }
    }
}

/// The prefix that keeps a local key out of every installed World's namespace.
///
/// A World id is reverse-domain and lowercase, so `local/` cannot collide with
/// one: `/` is not admitted in an id. That is the whole guarantee — a local
/// entry and an installed entry can never answer to the same key, whatever the
/// tree declares about itself.
pub const PREFIX: &str = "local/";

/// The key a registration is filed under.
///
/// Derived from the caller's chosen handle rather than from the tree, because
/// the tree can change what it declares — and an entry whose key moved when
/// somebody edited `world.json` is an entry that cannot be removed by the name
/// it was added under.
pub fn key_for(handle: &str) -> Result<String> {
    let handle = handle.trim();
    if handle.is_empty() {
        bail!("a local World needs a name to be filed under");
    }
    // Narrower than a filename needs to be, because this handle becomes a
    // mount, and a mount becomes an MCP tool prefix. `local_issues_list` has to
    // read as a tool name to whatever is looking at it.
    if !handle
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        bail!("'{handle}' may use lowercase letters, digits and '_' only");
    }
    Ok(format!("{PREFIX}{handle}"))
}

/// Where a local World is mounted, from the handle it was registered under.
///
/// The mount is not decoration and this is not a routing detail: it prefixes
/// every public MCP tool name and it is the `{world}` segment of the HTTP RPC
/// route. So a local World assigned `local_issues` exposes `local_issues_list`
/// and answers at `/local_issues/`, and an agent or a link addressing the
/// released World reaches the released World. That is the point — the two run
/// side by side and neither can be mistaken for the other by anything that
/// resolves by name.
///
/// A released World's `MOUNT` is published API and never moves; this is only
/// ever assigned to a tree somebody is working on.
pub fn mount_for(handle: &str) -> Result<String> {
    key_for(handle)?;
    Ok(format!("{MOUNT_PREFIX}{}", handle.trim()))
}

/// Reserved, so that a mount assigned here can never collide with one a World
/// declares for itself. A World that claims it is refused rather than quietly
/// shadowing somebody's working tree.
pub const MOUNT_PREFIX: &str = "local_";

fn file_for(identity: &Path, key: &str) -> Result<PathBuf> {
    let handle = key
        .strip_prefix(PREFIX)
        .ok_or_else(|| anyhow::anyhow!("'{key}' is not a local World key"))?;
    // Re-proved rather than trusted: the key reaches here from a surface, and
    // a handle carrying a separator would file this entry somewhere else
    // entirely.
    key_for(handle)?;
    Ok(registrations_root(identity).join(format!("{handle}.json")))
}

/// Register a tree as a local World.
///
/// Refuses at the moment of the act, so the refusal lands on whoever is asking
/// rather than on a World that fails to load an hour later. What it will not
/// do is decide whether the tree *works* — that is the loader's answer and it
/// belongs to the loader, which will say it in one voice for released and
/// local trees alike.
pub fn register(identity: &Path, handle: &str, dir: &Path) -> Result<String> {
    let key = key_for(handle)?;
    if !dir.is_absolute() {
        bail!("{} is not an absolute path", dir.display());
    }
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    // A tree with no declaration is not a World, and finding that out now is
    // the difference between a refusal and a Library row that never loads.
    let declared = dir.join("world.json");
    if !declared.is_file() {
        bail!(
            "{} has no world.json — a local World is an unsealed World tree, \
             not a directory of pages",
            dir.display()
        );
    }
    let root = registrations_root(identity);
    std::fs::create_dir_all(&root).context("create the local World registry")?;
    let encoded = serde_json::to_vec_pretty(&Registration {
        dir: dir.to_path_buf(),
    })
    .context("encode the local World registration")?;
    std::fs::write(file_for(identity, &key)?, encoded)
        .context("write the local World registration")?;
    Ok(key)
}

/// Forget a local World. Forgetting one that is not registered is not a
/// failure — the control is offered whether or not it is there.
pub fn forget(identity: &Path, key: &str) -> Result<()> {
    match std::fs::remove_file(file_for(identity, key)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("forget the local World"),
    }
}

/// Every local World registered on this device, by key.
///
/// A registration whose tree has gone is still listed, with no manifest. It is
/// a row that says it cannot be read, which is a different fact from not being
/// registered — and the only one of the two somebody can act on.
pub fn list(identity: &Path) -> Vec<Local> {
    let root = registrations_root(identity);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut found: Vec<Local> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let handle = name.strip_suffix(".json")?;
            let key = key_for(handle).ok()?;
            let bytes = std::fs::read(entry.path()).ok()?;
            let registration: Registration = serde_json::from_slice(&bytes).ok()?;
            let manifest = std::fs::read(registration.dir.join("world.json"))
                .ok()
                .and_then(|bytes| WorldManifest::parse(&bytes).ok());
            Some(Local {
                key,
                dir: registration.dir,
                manifest,
            })
        })
        .collect();
    found.sort_by(|a, b| a.key.cmp(&b.key));
    found
}

/// One local World, by key.
pub fn get(identity: &Path, key: &str) -> Option<Local> {
    list(identity).into_iter().find(|local| local.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(dir: &Path, declaration: Option<&str>) {
        std::fs::create_dir_all(dir).expect("a World tree");
        if let Some(declaration) = declaration {
            std::fs::write(dir.join("world.json"), declaration).expect("a declaration");
        }
    }

    fn declaration(id: &str) -> String {
        format!(
            r#"{{"format":1,"id":"{id}","version":"0.0.0-local","mount":"issues",
                 "name":"Issues","runners":[]}}"#
        )
    }

    #[test]
    fn a_registered_tree_is_listed_under_the_handle_it_was_added_with() {
        let identity = tempfile::tempdir().expect("an identity");
        let dir = tempfile::tempdir().expect("a tree");
        tree(dir.path(), Some(&declaration("com.lait.issues")));

        let key = register(identity.path(), "issues", dir.path()).expect("registers");
        assert_eq!(key, "local/issues");
        let found = list(identity.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "local/issues");
        assert_eq!(found[0].dir, dir.path());
    }

    /// The mount is what an agent types. `local_issues_list` is a different
    /// tool from `issues_list`, and `/local_issues/` is a different route from
    /// `/issues/` — which is what lets a tree somebody is working on run
    /// beside the release it was copied from without either answering for the
    /// other.
    #[test]
    fn a_local_world_is_mounted_in_its_own_namespace() {
        assert_eq!(mount_for("issues").expect("a mount"), "local_issues");
        assert!(mount_for("issues")
            .expect("a mount")
            .starts_with(MOUNT_PREFIX));
    }

    /// A handle becomes a mount and a mount becomes an MCP tool prefix, so it
    /// is held to what a tool name can carry rather than to what a filename
    /// can.
    #[test]
    fn a_handle_is_held_to_what_a_tool_name_can_carry() {
        assert!(key_for("issues_dev").is_ok());
        assert!(key_for("Issues").is_err(), "a tool prefix is lowercase");
        assert!(
            key_for("issues-dev").is_err(),
            "a hyphen does not read as one"
        );
        assert!(key_for("issues.dev").is_err());
    }

    /// The guarantee the whole namespace exists for. A World id is
    /// reverse-domain and lowercase and cannot contain `/`, so a local entry
    /// can never answer to an installed World's key however the tree names
    /// itself.
    #[test]
    fn a_local_key_can_never_collide_with_a_world_id() {
        let key = key_for("issues").expect("a key");
        assert!(key.starts_with(PREFIX));
        assert!(
            key.contains('/'),
            "the separator is the guarantee: no World id may contain one"
        );
        // And a handle that tries to escape the namespace is refused outright.
        assert!(key_for("../elsewhere").is_err());
        assert!(key_for("a/b").is_err());
        assert!(key_for("  ").is_err());
    }

    /// A directory of pages is not a World. Finding that out when somebody
    /// adds it is the difference between a refusal they can read and a Library
    /// row that never loads.
    #[test]
    fn a_tree_with_no_declaration_is_refused_when_it_is_added() {
        let identity = tempfile::tempdir().expect("an identity");
        let dir = tempfile::tempdir().expect("a directory of pages");
        tree(dir.path(), None);
        assert!(register(identity.path(), "issues", dir.path()).is_err());
        assert!(list(identity.path()).is_empty());
    }

    #[test]
    fn a_relative_tree_is_refused_because_nobody_means_the_daemons_cwd() {
        let identity = tempfile::tempdir().expect("an identity");
        assert!(register(identity.path(), "issues", Path::new("products/issues-app")).is_err());
    }

    /// Still listed, and saying so. A registration nobody can see is a
    /// registration nobody can remove — and "the tree is gone" is a different
    /// fact from "nothing is registered", with a different thing to do about
    /// it.
    #[test]
    fn a_registration_whose_tree_is_gone_is_still_a_row_that_says_so() {
        let identity = tempfile::tempdir().expect("an identity");
        let dir = tempfile::tempdir().expect("a tree");
        tree(dir.path(), Some(&declaration("com.lait.issues")));
        register(identity.path(), "issues", dir.path()).expect("registers");
        drop(dir);

        let found = list(identity.path());
        assert_eq!(found.len(), 1, "the registration outlives the tree");
        assert!(
            found[0].manifest.is_none(),
            "and says it cannot be read rather than claiming a name"
        );
        assert_eq!(found[0].display_name(), "local/issues");
    }

    #[test]
    fn forgetting_is_how_one_leaves_and_forgetting_nothing_is_not_a_failure() {
        let identity = tempfile::tempdir().expect("an identity");
        let dir = tempfile::tempdir().expect("a tree");
        tree(dir.path(), Some(&declaration("com.lait.issues")));
        register(identity.path(), "issues", dir.path()).expect("registers");

        forget(identity.path(), "local/issues").expect("forgets");
        assert!(list(identity.path()).is_empty());
        forget(identity.path(), "local/issues").expect("forgetting nothing is fine");
    }

    /// The key is the handle it was added under, never what the tree declares.
    /// A tree that renames itself must not take its registration's identity
    /// with it, or the entry cannot be removed by the name it was added under.
    #[test]
    fn editing_the_declaration_does_not_move_the_entry() {
        let identity = tempfile::tempdir().expect("an identity");
        let dir = tempfile::tempdir().expect("a tree");
        tree(dir.path(), Some(&declaration("com.lait.issues")));
        register(identity.path(), "issues", dir.path()).expect("registers");

        std::fs::write(
            dir.path().join("world.json"),
            declaration("com.example.something-else"),
        )
        .expect("the tree renames itself");
        let found = list(identity.path());
        assert_eq!(found[0].key, "local/issues", "the entry keeps its handle");
    }
}
