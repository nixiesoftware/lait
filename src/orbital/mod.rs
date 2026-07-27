//! The application's adoption of the orbital lifecycle.
//!
//! It fixes where the application keeps orbital stores, supplies the Mechanics
//! composition over signed Space material, and defines the product-neutral
//! [`WorldPackage`] / [`WorldBridge`] boundary used by SpaceBridge. Concrete
//! packages are created by the application composition root and injected
//! through LaitDaemon; this module does not construct or select IssuesWorld.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use replica::BodyKeySource;
use runtime::{AuthorityView, Runtime, WorldRegistry};

/// Where the application keeps its orbital stores, under the local home.
pub fn orbital_store_root(home: &Path) -> PathBuf {
    home.join("orbital")
}

/// A typed refusal for a pre-orbital home (clean break, no migration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedStoreVersion {
    /// Where the legacy store was detected.
    pub legacy_repo: PathBuf,
    /// Human recreation guidance.
    pub guidance: String,
}

impl std::fmt::Display for UnsupportedStoreVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unsupported store version at {}: {}",
            self.legacy_repo.display(),
            self.guidance
        )
    }
}

impl std::error::Error for UnsupportedStoreVersion {}

/// Whether `home` holds one formed/entered orbital Space store.
pub fn space_store_present(home: &Path) -> bool {
    discover_space_id(home).is_some()
}

/// The single orbital Space id under `home`, if any.
pub fn discover_space_id(home: &Path) -> Option<crate::ids::SpaceId> {
    let root = orbital_store_root(home);
    let mut found = None;
    for entry in std::fs::read_dir(&root).ok()?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        if let Some(space) = entry
            .file_name()
            .to_str()
            .filter(|name| name.starts_with("ws_"))
            .and_then(crate::ids::SpaceId::parse)
        {
            if found.replace(space).is_some() {
                return None;
            }
        }
    }
    found
}

/// Detect a pre-orbital (v0.x) store. A fresh Orbit is never created beside it.
pub fn unsupported_store_at(home: &Path) -> Option<UnsupportedStoreVersion> {
    let repo = home.join("repo");
    repo.join("genesis.json")
        .exists()
        .then(|| UnsupportedStoreVersion {
            legacy_repo: repo,
            guidance: "this home holds a pre-orbital space store; the orbital \
                       formats are a clean break with no migration. Export what \
                       you need with a v0.x binary, then remove the old store \
                       (or choose a fresh home) and re-create the space."
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        })
}

/// Open the generic Runtime at the application's orbital store convention.
pub fn open_orbital_runtime(
    home: &Path,
    registry: WorldRegistry,
    authority: Arc<dyn AuthorityView>,
    keys: Arc<dyn BodyKeySource>,
) -> Result<Runtime, UnsupportedStoreVersion> {
    if let Some(error) = unsupported_store_at(home) {
        return Err(error);
    }
    Ok(Runtime::open(
        orbital_store_root(home),
        registry,
        authority,
        keys,
    ))
}

pub mod ceremony;
pub mod mechanics;
pub mod space_bridge;
pub mod world_bridge;

pub use mechanics::{AuthorityRecord, OrbitalMechanics};
pub use space_bridge::{
    run_space_bridge, run_space_bridge_with, run_space_bridge_with_packages, SpaceBridge,
};
pub use world_bridge::{
    LegacyWorldCodec, WorldBridge, WorldBridgeRegistry, WorldBridgesBuilder, WorldCall,
    WorldCallAccess, WorldCallContext, WorldCallError, WorldCallErrorCode, WorldCallHandler,
    WorldPackage, WorldPackages, WorldReply,
};

// Compatibility exports for callers that reached the issue tracker's outer
// lifecycle adapter through `lait::orbital` before product ownership was made
// explicit.
pub use crate::world::lifecycle::{
    enter_space, form_space, form_space_with_fault, found_space_cli, issues_implementation_id,
    read_bootstrap_record, seed_founder_policy, BootstrapFault, BootstrapPhase,
    IssuesBootstrapRecord,
};

/// A random 16-byte value (salts, epoch ids, nonces).
pub(crate) fn rand16() -> [u8; 16] {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).expect("getrandom");
    raw
}
