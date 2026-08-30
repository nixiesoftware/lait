//! The Orbit catalog persisted in `spaces.json` (see `docs/UI.md`, joining).
//!
//! A small global index, `spaces.json` under [`crate::config::config_root`],
//! mapping each **store path** to the space it holds, so founders and joiners
//! alike are listed by `GET /api/spaces` and addressable via `--orbit`. It
//! carries **no secrets and no trust** (the signed ACL still gates every op);
//! a corrupt or absent file degrades to "no known spaces".
//!
//! **The registry persists coordinates, never contents.** A coordinate is a
//! fact this device owns and no other node can change: the space id, the store
//! path, whether we founded or joined, the inviter's nick, and when we last
//! opened it. Contents — the space's name, its projects — belong to the
//! Catalog, and a copy kept here can only ever be a cache that nothing
//! invalidates.
//!
//! That is not hypothetical. A `projects` snapshot lived here, written once at
//! founding and never refreshed, and it still named a space's single genesis
//! project years of work later. The name beside it was saved by a live probe
//! that happened to answer; when the probe missed, the founding name was served
//! in its place and no surface could tell the difference. Both fields were
//! documented as "refreshed on open" and there was no such refresh: [`upsert`]
//! is reached only from founding and entering.
//!
//! So the rule is structural rather than remembered. Contents are not fields
//! here, which is why they cannot drift. A surface that wants a name asks a
//! live Station and reports an absence when it cannot — see
//! [`crate::serve::orbits`].
//!
//! A v0.5.x `workspaces.json` beside this file is simply not read, and is not
//! migrated. That is the right outcome precisely because this is navigation
//! state: the registry rebuilds itself on the next founding, entry, or daemon
//! open, so a migration would buy nothing that opening a store once does not.

pub mod bootstrap;
mod catalog;
pub mod observed;
mod router;

pub use catalog::Catalog;
pub(crate) use catalog::{ResolvedOrbit, StationIdentity};
pub(crate) use router::{BlockingFailure, ContentPlacement};
pub use router::{Hosting, OrbitDoorbell, OrbitVacancy, Placement, Router, SlotVacancy};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::config_root;

/// Where a store's space came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Founded here (this node minted the genesis).
    Founded,
    /// Bootstrapped from someone else's invite.
    #[default]
    Joined,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Origin::Founded => "founded",
            Origin::Joined => "joined",
        })
    }
}

/// Advisory snapshot of one project, for cross-space listings. Display
/// only — the authoritative list is the space's own catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectBrief {
    pub key: String,
    pub name: String,
}

/// One registered store: a path on this machine and the space it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The space id (`ws_…`) bound in this store.
    pub space: String,
    /// The name recorded when this device founded or entered the store — a
    /// device-owned historical fact, like `host_nick`, and the only thing a
    /// by-name `--orbit` selector can match without a running daemon.
    ///
    /// **Never a display value.** It is not refreshed, so it lags a rename for
    /// good: the Catalog owns the name, and a surface that wants one asks a
    /// live Station (`StatusInfo::name`) or reports that it could not.
    #[serde(default)]
    pub name: String,
    /// The absolute store path (the `.lait/` dir, or a `$LAIT_HOME`).
    pub path: String,
    /// Founded here vs joined from an invite.
    #[serde(default)]
    pub origin: Origin,
    /// The inviter's nick from the ticket (joined only). May be empty.
    #[serde(default)]
    pub host_nick: String,
    /// Unix seconds of the last open — newest-first ordering. Written by
    /// [`touch`] when a Station begins serving this store.
    #[serde(default)]
    pub last_opened: u64,
}

/// Filesystem-level status of a registered entry. Whether a daemon is *up* is
/// a live control-channel probe, done by the client layer (it needs async).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The path holds a formed/entered Space store.
    Present,
    /// The path is gone or no longer holds a Space store.
    Missing,
}

/// Check whether an entry's path still holds a Space store, without opening
/// (or creating) anything.
pub fn presence(entry: &Entry) -> Presence {
    let path = Path::new(&entry.path);
    if crate::orbital::space_store_present(path) {
        Presence::Present
    } else {
        Presence::Missing
    }
}

/// Path to the registry file (`config_root/spaces.json`).
pub fn registry_file() -> Result<PathBuf> {
    Ok(config_root()?.join("spaces.json"))
}

/// The registry belonging to one stack, whichever stack this process is.
fn registry_file_for(profile: &crate::config::Profile) -> Result<PathBuf> {
    Ok(profile.config_root()?.join("spaces.json"))
}

/// Which stack registered this store, when one did.
///
/// A store is registered in exactly one profile's catalog — founding refuses
/// an occupied directory, and entering bootstraps a fresh home — so this is a
/// single answer, not a preference.
///
/// It is what makes an agent's discovery work across stacks. An editor spawns
/// `lait mcp` with an environment the editor controls, so the process cannot
/// be told which stack its repository belongs to; but the *store on disk* was
/// registered by exactly one of them, and asking which is a read of files that
/// are true whether or not any client is running. Without it, an agent that
/// found a store by walking up from its working directory would then route its
/// every call to whichever stack the editor happened to launch it in — and be
/// answered "no such local Orbit" by a daemon that has never heard of the
/// directory the agent is sitting in.
pub fn owner_of(store: &Path) -> Option<crate::config::Profile> {
    let wanted = crate::config::canonical(store);
    crate::config::profile::all().into_iter().find(|profile| {
        let Ok(path) = registry_file_for(profile) else {
            return false;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return false;
        };
        let entries: Vec<Entry> = serde_json::from_str(&raw).unwrap_or_default();
        entries
            .iter()
            .any(|entry| crate::config::canonical(Path::new(&entry.path)) == wanted)
    })
}

/// Read the registry, newest-first. Best-effort: a missing or corrupt file
/// yields an empty list rather than an error (navigation state, never a gate).
pub fn list() -> Vec<Entry> {
    let Ok(path) = registry_file() else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    entries.sort_by_key(|e| std::cmp::Reverse(e.last_opened));
    entries
}

fn save(entries: &[Entry]) -> Result<()> {
    let path = registry_file()?;
    let json = serde_json::to_string_pretty(entries).context("encode space registry")?;
    // Write atomically (temp file + rename) so a concurrent reader never
    // observes a half-written, unparseable file and wrongly concludes "no known
    // spaces". `rename` replaces the destination atomically on both unix
    // and Windows (std uses MOVEFILE_REPLACE_EXISTING).
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("commit {}", path.display()))?;
    Ok(())
}

/// Insert or refresh an entry, keyed by **store path** (one store holds exactly
/// one space). A refresh preserves fields the caller didn't recompute: an
/// empty `name`/`host_nick` on the new entry keeps the old value, and
/// `origin` sticks once founded (a daemon-open upsert must not relabel a founder
/// as joined). Best-effort persistence; callers treat failure as non-fatal (the
/// registry is a convenience, not a source of truth).
pub fn upsert(mut entry: Entry) -> Result<()> {
    let mut entries = list();
    if let Some(old) = entries.iter().find(|e| e.path == entry.path) {
        // Same store re-registered: merge, don't blank.
        if old.space == entry.space {
            if entry.name.is_empty() {
                entry.name = old.name.clone();
            }
            if entry.host_nick.is_empty() {
                entry.host_nick = old.host_nick.clone();
            }
            if old.origin == Origin::Founded {
                entry.origin = Origin::Founded;
            }
        }
        // A different space at the same path (re-init after rm) replaces
        // the row wholesale.
    }
    entries.retain(|e| e.path != entry.path);
    entries.push(entry);
    save(&entries)
}

/// Record that an application successfully opened this Orbit.
///
/// This timestamp is navigation history, not a liveness sample: merely serving
/// a World must not make it look freshly opened. A missing registry row is left
/// missing rather than fabricating one without a known store path.
pub fn touch(space: &str) -> Result<bool> {
    let mut entries = list();
    let Some(entry) = entries.iter_mut().find(|entry| entry.space == space) else {
        return Ok(false);
    };
    entry.last_opened = mechanics::wallclock::now_secs();
    save(&entries)?;
    Ok(true)
}

/// Deregister entries matching `sel` (exact path, exact space id, or a
/// **unique** space-id prefix — an ambiguous prefix removes nothing, so a
/// stray `forget ws_` can never wipe the registry). Never touches the store on
/// disk. Returns the removed entries.
pub fn forget(sel: &str) -> Result<Vec<Entry>> {
    let entries = list();
    // Two spellings can name one directory, and a row is written canonicalized
    // while a caller names the store however they reached it — under an 8.3
    // alias or a symlinked temp dir those strings never compare equal. String
    // equality stays first and stays authoritative: a row whose store is gone
    // resolves to nothing, and deregistering exactly that row is what `forget`
    // is for.
    let target = crate::config::resolved(Path::new(sel));
    let exact = |e: &Entry| {
        e.path == sel
            || e.space == sel
            || target
                .as_deref()
                .is_some_and(|t| crate::config::resolved(Path::new(&e.path)).as_deref() == Some(t))
    };
    let matches_exact = entries.iter().filter(|e| exact(e)).count();
    let prefix_hits = entries
        .iter()
        .filter(|e| sel.starts_with("ws_") && e.space.starts_with(sel))
        .count();
    let (removed, kept): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| {
        exact(e) || (matches_exact == 0 && prefix_hits == 1 && e.space.starts_with(sel))
    });
    if !removed.is_empty() {
        save(&kept)?;
    }
    Ok(removed)
}

/// Drop every entry whose path no longer holds an initialized store. Returns
/// the removed entries.
pub fn prune() -> Result<Vec<Entry>> {
    let entries = list();
    let (removed, kept): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|e| presence(e) == Presence::Missing);
    if !removed.is_empty() {
        save(&kept)?;
    }
    Ok(removed)
}

/// Why a selector did not name exactly one local Orbit.
///
/// Typed, because the CLI used to answer the ambiguous case by printing the
/// candidates and calling `process::exit(2)` from inside a resolver. A head
/// that serves many callers out of one process cannot exit, and a browser
/// cannot read a line written to its server's stderr — so the candidates are
/// carried in the value and the surface decides what to do with them.
#[derive(Debug)]
pub enum Unresolved {
    /// A path-shaped selector that holds no space store.
    NoStoreAt { selector: String },
    /// Nothing in the registry matches.
    NoMatch {
        selector: String,
        known: Vec<String>,
    },
    /// More than one entry matches.
    Ambiguous {
        selector: String,
        candidates: Vec<String>,
    },
    /// Exactly one entry matches, but its store is gone from disk.
    Missing { selector: String, path: String },
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unresolved::NoStoreAt { selector } => write!(
                f,
                "no initialized space store at '{selector}' (or under '{selector}/.lait')"
            ),
            Unresolved::NoMatch { selector, known } => write!(
                f,
                "no Orbit matches '{selector}' — known: {}",
                if known.is_empty() {
                    "(none)".to_string()
                } else {
                    known.join(", ")
                }
            ),
            Unresolved::Ambiguous {
                selector,
                candidates,
            } => write!(
                f,
                "Orbit selector '{selector}' is ambiguous:\n  {}",
                candidates.join("\n  ")
            ),
            Unresolved::Missing { selector, path } => write!(
                f,
                "Orbit '{selector}' is registered at {path} but the store is gone \
                 — prune the registry"
            ),
        }
    }
}
impl std::error::Error for Unresolved {}

/// Resolve a selector to one durable local Orbit's store path: a filesystem
/// path, an `orb_` id prefix, a `ws_` Space id prefix, or a case-insensitive
/// display-name match.
///
/// Two entries may legitimately participate in the same Space, so an ambiguous
/// selector is an answer with candidates rather than a guess.
pub fn select(selector: &str) -> std::result::Result<PathBuf, Unresolved> {
    // Path form: explicit separators or an existing directory. Accept either
    // the `.lait` dir itself or its parent.
    if selector.contains('/') || selector.contains('\\') || Path::new(selector).is_dir() {
        // Resolved, not echoed back as spelled. Registration writes the row
        // through `prepare_store_dir`, which canonicalizes — so handing a
        // caller's spelling straight to a registry lookup finds nothing the
        // moment the two differ, which on Windows is any path under an 8.3
        // alias (`RUNNER~1`) and on macOS any path through a symlinked temp dir.
        let candidate = Path::new(selector);
        if crate::orbital::space_store_present(candidate) {
            return Ok(crate::config::canonical(candidate));
        }
        let nested = candidate.join(".lait");
        if crate::orbital::space_store_present(&nested) {
            return Ok(crate::config::canonical(&nested));
        }
        return Err(Unresolved::NoStoreAt {
            selector: selector.to_string(),
        });
    }

    let entries = list();
    let matches: Vec<&Entry> = if selector.starts_with("orb_") {
        entries
            .iter()
            .filter(|e| {
                crate::daemon::LocalOrbitId::for_store(Path::new(&e.path))
                    .as_str()
                    .starts_with(selector)
            })
            .collect()
    } else if selector.starts_with("ws_") {
        entries
            .iter()
            .filter(|e| e.space == selector || e.space.starts_with(selector))
            .collect()
    } else {
        entries
            .iter()
            .filter(|e| e.name.eq_ignore_ascii_case(selector))
            .collect()
    };

    match matches.as_slice() {
        [only] => {
            if presence(only) == Presence::Missing {
                return Err(Unresolved::Missing {
                    selector: selector.to_string(),
                    path: only.path.clone(),
                });
            }
            Ok(PathBuf::from(&only.path))
        }
        [] => Err(Unresolved::NoMatch {
            selector: selector.to_string(),
            known: entries
                .iter()
                .map(|e| {
                    if e.name.is_empty() {
                        e.space.clone()
                    } else {
                        e.name.clone()
                    }
                })
                .collect(),
        }),
        many => Err(Unresolved::Ambiguous {
            selector: selector.to_string(),
            candidates: many
                .iter()
                .map(|e| format!("{} ({})", e.space, e.path))
                .collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::poison::LockRecovering;
    use std::sync::{Mutex, MutexGuard};

    // `LAIT_CONFIG_ROOT` is process-global, so these tests can't run concurrently:
    // one setting the env would clobber another mid-flight. Serialize them behind a
    // lock (held for the whole test) rather than hoping the scheduler cooperates.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Point `config_root` at a fresh scratch dir for the duration of a test, while
    /// holding the env lock so no other test observes our `LAIT_CONFIG_ROOT`.
    struct ScopedRoot {
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }
    impl ScopedRoot {
        fn new(tag: &str) -> Self {
            let guard = ENV_LOCK.lock_recovering();
            let dir =
                std::env::temp_dir().join(format!("lait-wsreg-{}-{}", tag, std::process::id(),));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("LAIT_CONFIG_ROOT", &dir);
            ScopedRoot { dir, _guard: guard }
        }
    }
    impl Drop for ScopedRoot {
        fn drop(&mut self) {
            std::env::remove_var("LAIT_CONFIG_ROOT");
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    fn entry(space: &str, path: &str, last_opened: u64) -> Entry {
        Entry {
            space: space.into(),
            name: "demo".into(),
            path: path.into(),
            origin: Origin::Joined,
            host_nick: "host".into(),
            last_opened,
        }
    }

    #[test]
    fn upsert_then_list_returns_the_entry() {
        let _root = ScopedRoot::new("basic");
        upsert(entry("ws_A", "/tmp/a", 10)).unwrap();
        let got = list();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].space, "ws_A");
    }

    #[test]
    fn upsert_is_keyed_by_path_not_duplicated() {
        let _root = ScopedRoot::new("dedup");
        upsert(entry("ws_A", "/tmp/a", 10)).unwrap();
        // Same path, different space (re-init after rm) → replace, not duplicate.
        upsert(entry("ws_B", "/tmp/a", 20)).unwrap();
        let got = list();
        assert_eq!(got.len(), 1, "same path must not create a second row");
        assert_eq!(got[0].space, "ws_B", "re-register replaces the row");
    }

    /// The persisted shape is the guard rail.
    ///
    /// `name` is here because a by-name `--orbit` selector has nothing else to
    /// match before a daemon is up, and it is documented as a formation-time
    /// hint no surface may display. Everything else is a coordinate. A field
    /// naming Catalog content — a project list, a description, a member count —
    /// must not join them: nothing here is ever refreshed, so it would be wrong
    /// from the first rename onward and no surface could tell.
    #[test]
    fn a_row_persists_coordinates_and_no_contents() {
        let row = serde_json::to_value(entry("ws_A", "/tmp/a", 10)).expect("encode an entry");
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("an entry encodes as an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["host_nick", "last_opened", "name", "origin", "path", "space"],
            "the registry persists coordinates; a new field here is Catalog              content unless it is a fact this device owns"
        );
    }

    #[test]
    fn refresh_merges_instead_of_blanking() {
        let _root = ScopedRoot::new("merge");
        let mut founded = entry("ws_A", "/tmp/a", 10);
        founded.origin = Origin::Founded;
        upsert(founded).unwrap();
        // A later daemon-open upsert that didn't recompute the name and
        // defaulted origin must keep the founded origin and the old name.
        upsert(Entry {
            space: "ws_A".into(),
            name: String::new(),
            path: "/tmp/a".into(),
            origin: Origin::Joined,
            host_nick: String::new(),
            last_opened: 20,
        })
        .unwrap();
        let got = list();
        assert_eq!(got[0].origin, Origin::Founded, "founded origin sticks");
        assert_eq!(got[0].name, "demo", "empty name keeps the old value");
        assert_eq!(got[0].last_opened, 20, "freshness does update");

        assert!(touch("ws_A").unwrap(), "a known Orbit can be touched");
        assert!(
            list()[0].last_opened >= 20,
            "an application open refreshes its history"
        );
        assert!(
            !touch("ws_missing").unwrap(),
            "history must not fabricate an unregistered Orbit"
        );
        assert_eq!(list().len(), 1);
    }

    #[test]
    fn list_is_newest_first() {
        let _root = ScopedRoot::new("order");
        upsert(entry("ws_old", "/tmp/old", 5)).unwrap();
        upsert(entry("ws_new", "/tmp/new", 50)).unwrap();
        let got = list();
        assert_eq!(got[0].space, "ws_new", "newest last_opened sorts first");
    }

    #[test]
    fn missing_registry_is_empty_not_an_error() {
        let _root = ScopedRoot::new("empty");
        assert!(list().is_empty());
    }

    #[test]
    fn forget_removes_by_path_or_id_prefix() {
        let _root = ScopedRoot::new("forget");
        upsert(entry("ws_AAAA", "/tmp/a", 10)).unwrap();
        upsert(entry("ws_BBBB", "/tmp/b", 20)).unwrap();
        // An ambiguous prefix removes NOTHING (a stray `forget ws_` must never
        // wipe the registry).
        assert!(forget("ws_").unwrap().is_empty());
        assert_eq!(list().len(), 2);
        assert_eq!(forget("/tmp/a").unwrap().len(), 1);
        assert_eq!(list().len(), 1);
        assert_eq!(
            forget("ws_BB").unwrap().len(),
            1,
            "unique id prefix matches"
        );
        assert!(list().is_empty());
    }

    #[test]
    fn prune_drops_only_missing_stores() {
        let _root = ScopedRoot::new("prune");
        // A real initialized store on disk…
        let live = std::env::temp_dir().join(format!("lait-wsreg-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&live);
        // The store probe wants a `ws_*` directory under the orbital store
        // root — shape only, no contents.
        std::fs::create_dir_all(live.join("orbital").join("ws_00000000000000000000000000"))
            .unwrap();
        upsert(entry("ws_LIVE", live.to_str().unwrap(), 10)).unwrap();
        // …and a registered path that holds nothing.
        upsert(entry("ws_GONE", "/tmp/definitely-gone-xyz", 20)).unwrap();
        let removed = prune().unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].space, "ws_GONE");
        let kept = list();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].space, "ws_LIVE");
        let _ = std::fs::remove_dir_all(&live);
    }
}
