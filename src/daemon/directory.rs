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

use crate::ids::SpaceId;
use crate::orbits::{self, Entry};

use super::{ClientScope, LocalOrbitId, OrbitAddress};

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
/// activates a Station. Activation belongs to the [`super::ControlRouter`].
pub struct OrbitDirectory {
    identity: PathBuf,
    agents_base: PathBuf,
    self_contained: bool,
    load_bindings: Arc<dyn Fn() -> Vec<Entry> + Send + Sync>,
}

impl OrbitDirectory {
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
        Self {
            identity,
            agents_base,
            self_contained,
            load_bindings,
        }
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
            projects: Vec::new(),
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
