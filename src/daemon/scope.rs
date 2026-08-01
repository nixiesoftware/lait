use std::collections::BTreeSet;
use std::path::Path;

use mechanics::ids::SpaceId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const LOCAL_ORBIT_PREFIX: &str = "orb_";
const LOCAL_ORBIT_HEX_LEN: usize = 64;

/// Stable local address of one durable Orbit binding.
///
/// This is deliberately not a [`SpaceId`]. Two store paths may hold distinct
/// local Orbits in the same Space, possibly under different Station identities.
/// The id is a full BLAKE3 digest of the normalized store path; unlike the old
/// short viewer handle, it is suitable as a map key and route component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalOrbitId(String);

impl LocalOrbitId {
    /// Derive the stable local Orbit id for a store binding.
    ///
    /// Registry paths are absolute, but the normalization also makes separator
    /// and Windows case drift harmless. It does not canonicalize through the
    /// filesystem, so an id remains derivable after the store goes missing.
    pub fn for_store(path: &Path) -> Self {
        let normalized = normalize(path);
        let digest = blake3::derive_key("lait.local-orbit-id.v1", normalized.as_bytes());
        Self(format!(
            "{LOCAL_ORBIT_PREFIX}{}",
            data_encoding::HEXLOWER.encode(&digest)
        ))
    }

    pub fn parse(value: &str) -> Option<Self> {
        let hex = value.strip_prefix(LOCAL_ORBIT_PREFIX)?;
        if hex.len() == LOCAL_ORBIT_HEX_LEN
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Some(Self(value.to_string()))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LocalOrbitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for LocalOrbitId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LocalOrbitId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid local Orbit id '{value}'")))
    }
}

/// Complete local address of the Orbit through which a Space is reached.
///
/// `orbit` selects one local binding. `space` is repeated as an expectation so
/// stale catalogs or confused routes fail before reaching Mechanics or a World.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrbitAddress {
    pub orbit: LocalOrbitId,
    pub space: SpaceId,
}

impl OrbitAddress {
    pub fn for_store(path: &Path, space: SpaceId) -> Self {
        Self {
            orbit: LocalOrbitId::for_store(path),
            space,
        }
    }
}

/// The local Orbits one client is allowed to address.
///
/// This is intentionally not serialized in `ClientRequest`: a caller cannot
/// grant itself access by asserting a larger set. CLI/MCP construct a pinned
/// scope from their resolved home. The web adapter applies its broader identity
/// visibility policy before constructing a catalog-resolved route. The
/// Daemon resolves every explicit address through its own Catalog
/// and never accepts an allowed set as a wire claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientScope {
    default_orbit: Option<LocalOrbitId>,
    allowed_orbits: BTreeSet<LocalOrbitId>,
}

impl ClientScope {
    /// A single-Orbit scope for a cwd-bound CLI or MCP server.
    pub fn pinned(orbit: LocalOrbitId) -> Self {
        Self {
            default_orbit: Some(orbit.clone()),
            allowed_orbits: BTreeSet::from([orbit]),
        }
    }

    /// A catalog-style scope, such as the user's web viewer.
    pub fn catalog(
        default_orbit: Option<LocalOrbitId>,
        allowed_orbits: impl IntoIterator<Item = LocalOrbitId>,
    ) -> Result<Self, ScopeDenied> {
        let allowed_orbits: BTreeSet<_> = allowed_orbits.into_iter().collect();
        if let Some(default) = &default_orbit {
            if !allowed_orbits.contains(default) {
                return Err(ScopeDenied::DefaultNotAllowed(default.clone()));
            }
        }
        Ok(Self {
            default_orbit,
            allowed_orbits,
        })
    }

    pub fn default_orbit(&self) -> Option<&LocalOrbitId> {
        self.default_orbit.as_ref()
    }

    pub fn allows(&self, address: &OrbitAddress) -> bool {
        self.allowed_orbits.contains(&address.orbit)
    }

    pub fn authorize(&self, address: &OrbitAddress) -> Result<(), ScopeDenied> {
        if self.allows(address) {
            Ok(())
        } else {
            Err(ScopeDenied::OrbitNotAllowed(address.orbit.clone()))
        }
    }

    pub fn allowed_orbits(&self) -> impl Iterator<Item = &LocalOrbitId> {
        self.allowed_orbits.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDenied {
    DefaultNotAllowed(LocalOrbitId),
    OrbitNotAllowed(LocalOrbitId),
}

impl std::fmt::Display for ScopeDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DefaultNotAllowed(orbit) => {
                write!(
                    f,
                    "default Orbit {orbit} is not in the client's allowed set"
                )
            }
            Self::OrbitNotAllowed(orbit) => {
                write!(f, "client is not allowed to address local Orbit {orbit}")
            }
        }
    }
}

impl std::error::Error for ScopeDenied {}

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

    fn space(marker: u8) -> SpaceId {
        SpaceId::from_digest([marker; 16])
    }

    #[test]
    fn local_orbit_ids_are_stable_path_distinct_and_strictly_parsed() {
        let a = LocalOrbitId::for_store(Path::new("/home/u/a/.lait"));
        let again = LocalOrbitId::for_store(Path::new("/home/u/a/.lait/"));
        let b = LocalOrbitId::for_store(Path::new("/home/u/b/.lait"));
        assert_eq!(a, again);
        assert_ne!(a, b);
        assert_eq!(LocalOrbitId::parse(a.as_str()), Some(a.clone()));
        assert!(LocalOrbitId::parse("orb_short").is_none());
        assert!(LocalOrbitId::parse(&a.as_str().to_ascii_uppercase()).is_none());
    }

    #[test]
    fn two_local_orbits_in_one_space_remain_distinct_and_scope_is_pinned() {
        let shared_space = space(7);
        let a = OrbitAddress::for_store(Path::new("/home/u/a/.lait"), shared_space.clone());
        let b = OrbitAddress::for_store(Path::new("/home/u/b/.lait"), shared_space);
        assert_ne!(a.orbit, b.orbit);

        let scope = ClientScope::pinned(a.orbit.clone());
        assert_eq!(scope.default_orbit(), Some(&a.orbit));
        assert!(scope.authorize(&a).is_ok());
        assert_eq!(
            scope.authorize(&b),
            Err(ScopeDenied::OrbitNotAllowed(b.orbit))
        );
    }

    #[test]
    fn a_catalog_default_must_also_be_allowed() {
        let a = LocalOrbitId::for_store(Path::new("/a"));
        let b = LocalOrbitId::for_store(Path::new("/b"));
        assert_eq!(
            ClientScope::catalog(Some(a.clone()), [b]),
            Err(ScopeDenied::DefaultNotAllowed(a))
        );
    }
}
