//! Discovery and authorization of durable local Orbit bindings.
//!
//! The directory is host-plane infrastructure. It knows which local stores are
//! visible to the daemon's identity and resolves a stable [`LocalOrbitId`] to
//! the expected Space and Station identity. It does not activate a Station,
//! open product state, or decide how an Orbit is hosted.
//!
//! Identity is global by default, so ordinary stores share the host identity.
//! A self-contained agent home is the exception: the human-scoped directory may
//! observe it but preserves its distinct identity binding, while an
//! agent-scoped directory can enumerate only itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::orbits::{self, Entry};
use mechanics::ids::SpaceId;

use crate::daemon::{ClientScope, LocalOrbitId, OrbitAddress};

/// Whose key the Station placed in an Orbit signs with.
///
/// Identity is global by default. A named agent is a self-contained home and
/// therefore carries a distinct identity binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StationIdentity {
    /// The identity the host daemon itself runs as.
    Own,
    /// A named agent's self-contained identity.
    Agent { name: String },
}

/// One visible durable Orbit binding from the local registry.
#[derive(Debug, Clone)]
pub struct OrbitBinding {
    pub entry: Entry,
    pub identity: StationIdentity,
}

/// One visible and authorized local Orbit resolved for control routing.
#[derive(Debug, Clone)]
pub struct ResolvedOrbit {
    pub home: PathBuf,
    /// Directory containing the identity seed this Station must use.
    pub identity_dir: PathBuf,
    pub address: OrbitAddress,
    pub identity: StationIdentity,
}

/// The daemon's view of durable local Orbit bindings.
///
/// Listing is deliberately passive: it reads registry metadata but never
/// activates a Station. Activation belongs to the [`crate::orbits::Router`].
pub struct Catalog {
    identity: PathBuf,
    agents_base: PathBuf,
    self_contained: bool,
    load_bindings: Arc<dyn Fn() -> Vec<Entry> + Send + Sync>,
}

impl Catalog {
    pub fn new(identity: PathBuf, agents_base: PathBuf, self_contained: bool) -> Self {
        Self::with_loader(
            identity,
            agents_base,
            self_contained,
            Arc::new(orbits::list),
        )
    }

    fn with_loader(
        identity: PathBuf,
        agents_base: PathBuf,
        self_contained: bool,
        load_bindings: Arc<dyn Fn() -> Vec<Entry> + Send + Sync>,
    ) -> Self {
        // Deliberately stored as spelled, not canonicalized. Resolving these two
        // here while registry rows arrive as written is drift, not a cure for it:
        // on a volume with 8.3 aliasing (`RUNNER~1`) or through a symlinked temp
        // dir, `entry.path` and a resolved `identity` stop matching and every
        // binding disappears behind "no such space". Spelling is reconciled where
        // the two are compared — see `agent_name` — so both operands always move
        // together.
        Self {
            identity,
            agents_base,
            self_contained,
            load_bindings,
        }
    }

    /// A catalog over a caller-supplied registry read.
    ///
    /// Test-only, and gated so it stays that way: the registry is one file
    /// per machine, and this is what lets two Routers in one test process
    /// each see the stores that are theirs. Production reads the file.
    #[cfg(test)]
    pub(crate) fn with_registry_view(
        identity: PathBuf,
        agents_base: PathBuf,
        self_contained: bool,
        load_bindings: Arc<dyn Fn() -> Vec<Entry> + Send + Sync>,
    ) -> Self {
        Self::with_loader(identity, agents_base, self_contained, load_bindings)
    }

    #[cfg(test)]
    pub(crate) fn with_entries(
        identity: PathBuf,
        agents_base: PathBuf,
        self_contained: bool,
        entries: Vec<Entry>,
    ) -> Self {
        Self::with_loader(
            identity,
            agents_base,
            self_contained,
            Arc::new(move || entries.clone()),
        )
    }

    /// The directory holding the seed this directory's own Stations sign with.
    ///
    /// Formation needs it before any Orbit exists to resolve it from, which is
    /// the whole reason the daemon can host `HostSpaceFound` at all.
    pub fn identity(&self) -> &Path {
        &self.identity
    }

    /// Return the Orbits visible to this daemon identity, preserving registry
    /// order. Human observability includes named agents; self-contained
    /// identities see only their own home. No control channel is opened and no
    /// Station is activated.
    pub fn bindings(&self) -> Vec<OrbitBinding> {
        visible_bindings(
            (self.load_bindings)(),
            &self.identity,
            &self.agents_base,
            self.self_contained,
        )
    }

    /// Whether a Station placed in this Orbit signs with the seed this
    /// directory's own callers already hold, rather than with a separate one
    /// this daemon merely hosts.
    ///
    /// A custody question, deliberately not a question about what kind of
    /// member the Orbit belongs to. Whether a holder may write is decided by
    /// their grants; this decides only whose *key* would make the signature,
    /// and a key that is not the caller's is not theirs to spend however
    /// generous their grants become.
    pub fn signs_with_own_seed(&self, resolved: &ResolvedOrbit) -> bool {
        same_path(&resolved.identity_dir, &self.identity)
    }

    /// The same custody question asked of a bare path, before any registry row
    /// exists to resolve.
    ///
    /// Formation is the one caller that needs it a step early. Entering a store
    /// is what *registers* it, so a re-entry aimed at an unregistered home has
    /// no binding to ask about — and by the time one exists, that home's config
    /// has been written and its seed is about to sign a `Connect` on the wire.
    /// Where a home lives is what decides whose key it holds, and that is
    /// knowable without the registry.
    ///
    /// *Where* a directory lives is a question about the filesystem, not about
    /// the string a caller spelled it with, so a spelling the filesystem cannot
    /// resolve is refused rather than compared. `<home>/nope/../agents/scout`
    /// does not start with `agents/` as text and is that agent's home on disk;
    /// answering from the text let a caller-named path walk past this gate and
    /// then be materialized into the very directory it names. A caller with a
    /// directory that does not exist yet materializes it first — which is what
    /// [`crate::orbits::bootstrap`] does — so the gate and the write cannot
    /// disagree about which directory they mean.
    pub fn path_signs_with_own_seed(&self, home: &Path) -> bool {
        let Some(home) = crate::config::resolved(home) else {
            return false;
        };
        if self.self_contained {
            // A self-contained identity is visible only to itself, so any other
            // directory is somebody else's regardless of where it sits.
            return same_path(&home, &crate::config::canonical(&self.identity));
        }
        agent_name(&home, &crate::config::canonical(&self.agents_base)).is_none()
    }

    /// Resolve and authorize a stable local Orbit id.
    ///
    /// A binding outside this directory's identity scope is indistinguishable
    /// from a missing binding, so an agent cannot address a sibling by guessing
    /// its id.
    pub fn resolve(&self, id: &str) -> Result<ResolvedOrbit> {
        let id = LocalOrbitId::parse(id).ok_or_else(|| anyhow!("invalid local Orbit id"))?;
        let bindings = self.bindings();
        let client_scope = ClientScope::catalog(
            None,
            bindings
                .iter()
                .map(|binding| LocalOrbitId::for_store(Path::new(&binding.entry.path))),
        )?;
        let selected = bindings
            .into_iter()
            .find(|binding| LocalOrbitId::for_store(Path::new(&binding.entry.path)) == id)
            .ok_or_else(|| anyhow!("no such local Orbit"))?;
        let space = SpaceId::parse(&selected.entry.space)
            .ok_or_else(|| anyhow!("registered local Orbit has an invalid Space id"))?;
        let address = OrbitAddress { orbit: id, space };
        client_scope.authorize(&address)?;
        let home = PathBuf::from(&selected.entry.path);
        let identity_dir = match &selected.identity {
            StationIdentity::Own => self.identity.clone(),
            StationIdentity::Agent { .. } => home.clone(),
        };
        Ok(ResolvedOrbit {
            home,
            identity_dir,
            address,
            identity: selected.identity,
        })
    }
}

fn visible_bindings(
    entries: Vec<Entry>,
    identity: &Path,
    agents_base: &Path,
    self_contained: bool,
) -> Vec<OrbitBinding> {
    entries
        .into_iter()
        .filter_map(|entry| {
            let path = Path::new(&entry.path);
            let station_identity = if self_contained {
                if !same_path(path, identity) {
                    return None;
                }
                StationIdentity::Own
            } else if let Some(name) = agent_name(path, agents_base) {
                StationIdentity::Agent { name }
            } else {
                StationIdentity::Own
            };
            Some(OrbitBinding {
                entry,
                identity: station_identity,
            })
        })
        .collect()
}

/// The first component below `agents_base`, retaining its display case.
fn agent_name(path: &Path, agents_base: &Path) -> Option<String> {
    // Both operands move together or neither does. Resolving only one is worse
    // than resolving neither: on a volume with 8.3 aliasing a resolved candidate
    // stops starting with an unresolved base, and an agent's home would answer
    // "not an agent" — a custody question failing open.
    let pair = crate::config::resolved(path).zip(crate::config::resolved(agents_base));
    let (path, agents_base) = match &pair {
        Some((p, b)) => (p.as_path(), b.as_path()),
        None => (path, agents_base),
    };
    if !under(path, agents_base) {
        return None;
    }
    path.components()
        .nth(agents_base.components().count())
        .map(|component| component.as_os_str().to_string_lossy().to_string())
}

fn same_path(a: &Path, b: &Path) -> bool {
    normalize(a) == normalize(b)
}

fn under(path: &Path, base: &Path) -> bool {
    let (path, base) = (normalize(path), normalize(base));
    Path::new(&path).starts_with(Path::new(&base))
}

fn normalize(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    let value = value.trim_end_matches('/').to_string();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> Entry {
        Entry {
            space: "ws_test".into(),
            name: "Test".into(),
            path: path.into(),
            origin: orbits::Origin::Founded,
            host_nick: String::new(),
            last_opened: 0,
        }
    }

    fn paths(bindings: &[OrbitBinding]) -> Vec<&str> {
        bindings
            .iter()
            .map(|binding| binding.entry.path.as_str())
            .collect()
    }

    #[test]
    fn global_identity_sees_own_stores_and_agents_with_identity_preserved() {
        let bindings = visible_bindings(
            vec![
                entry("/home/u/proj-a/.lait"),
                entry("/home/u/.config/lait/agents/scout"),
                entry("/home/u/proj-b/.lait"),
            ],
            Path::new("/home/u/.config/lait"),
            Path::new("/home/u/.config/lait/agents"),
            false,
        );
        assert_eq!(
            paths(&bindings),
            vec![
                "/home/u/proj-a/.lait",
                "/home/u/.config/lait/agents/scout",
                "/home/u/proj-b/.lait"
            ]
        );
        assert_eq!(bindings[0].identity, StationIdentity::Own);
        assert_eq!(
            bindings[1].identity,
            StationIdentity::Agent {
                name: "scout".into()
            }
        );
        assert_eq!(bindings[2].identity, StationIdentity::Own);
    }

    #[test]
    fn self_contained_identity_sees_only_itself() {
        let bindings = visible_bindings(
            vec![
                entry("/home/u/proj-a/.lait"),
                entry("/home/u/.config/lait/agents/scout"),
                entry("/home/u/.config/lait/agents/other"),
            ],
            Path::new("/home/u/.config/lait/agents/scout"),
            Path::new("/home/u/.config/lait/agents"),
            true,
        );
        assert_eq!(paths(&bindings), vec!["/home/u/.config/lait/agents/scout"]);
        assert_eq!(bindings[0].identity, StationIdentity::Own);
    }

    #[test]
    fn nested_agent_binding_keeps_owner_name_and_case() {
        let bindings = visible_bindings(
            vec![entry("/home/u/.config/lait/agents/Scout/nested/store")],
            Path::new("/home/u/.config/lait"),
            Path::new("/home/u/.config/lait/agents"),
            false,
        );
        assert_eq!(
            bindings[0].identity,
            StationIdentity::Agent {
                name: "Scout".into()
            }
        );
    }

    /// Whose key would make the signature — asked about the key, not about the
    /// kind of member the Orbit belongs to. A holder's grants decide what they
    /// may do; they never make somebody else's seed theirs to spend.
    #[test]
    fn custody_asks_which_key_would_sign_and_nothing_about_the_holder() {
        let catalog = Catalog::with_entries(
            PathBuf::from("/home/u/.config/lait"),
            PathBuf::from("/home/u/.config/lait/agents"),
            false,
            Vec::new(),
        );
        let store = Path::new("/home/u/proj/.lait");
        let resolved = |identity_dir: &str, identity: StationIdentity| ResolvedOrbit {
            home: store.to_path_buf(),
            identity_dir: PathBuf::from(identity_dir),
            address: OrbitAddress::for_store(store, SpaceId::from_digest([7; 16])),
            identity,
        };

        assert!(
            catalog.signs_with_own_seed(&resolved("/home/u/.config/lait", StationIdentity::Own))
        );
        // Spelling drift is not a different key.
        assert!(
            catalog.signs_with_own_seed(&resolved("/home/u/.config/lait/", StationIdentity::Own))
        );
        // A seed this daemon merely hosts is somebody else's to spend.
        assert!(!catalog.signs_with_own_seed(&resolved(
            "/home/u/.config/lait/agents/scout",
            StationIdentity::Agent {
                name: "scout".into()
            }
        )));
    }

    #[test]
    fn path_comparison_handles_platform_spelling_without_prefix_confusion() {
        let bindings = visible_bindings(
            vec![entry(r"C:\Users\U\proj\.lait")],
            Path::new("C:/users/u/proj/.lait"),
            Path::new("C:/users/u/AppData/lait/agents"),
            true,
        );
        if cfg!(windows) {
            assert_eq!(bindings.len(), 1, "same dir, different spelling");
        }

        let bindings = visible_bindings(
            vec![entry("/home/u/.config/lait/agents-notreally/x")],
            Path::new("/home/u/.config/lait"),
            Path::new("/home/u/.config/lait/agents"),
            false,
        );
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].identity, StationIdentity::Own);
    }
}
