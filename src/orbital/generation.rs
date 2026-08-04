//! Application composition of an Orbit representation rebuild.

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;

use super::{discover_space, orbital_store_root, SpaceAuthority, SpaceStore};

/// The verified generation selected by a rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuild {
    pub generation: runtime::generation::Generation,
    pub effects: u64,
    pub bodies: u64,
    pub receipts: u64,
    pub evidence: [u8; 32],
}

/// Rebuild the implicit prior journal representation as an explicit current
/// generation and atomically select it. This is intentionally a local
/// representation operation: it authors no Space authority effect and changes
/// no World implementation.
pub fn rebuild_prior(home: &Path, device_seed: &[u8; 32]) -> Result<Rebuild> {
    let space = match discover_space(home) {
        SpaceStore::One(space) => space,
        SpaceStore::Absent => return Err(anyhow!("no orbital Space in this home")),
        SpaceStore::Several => {
            return Err(anyhow!(
                "this home holds more than one orbital Space; a rebuild has no way to pick one"
            ))
        }
    };
    let orbit = orbital_store_root(home).join(space.as_str());
    let active = runtime::generation::Active::read(&orbit)
        .map_err(|error| anyhow!("read active generation: {error}"))?;
    if let Some(generation) = active.generation() {
        return Err(anyhow!(
            "Orbit already uses explicit generation {generation}; this recipe only rebuilds the implicit prior representation"
        ));
    }

    // A generation build and a Station are mutually exclusive. Holding the
    // Orbit's existing operational lock also serializes rebuilds with daemon
    // startup; activation's own pointer lock supplies the final source CAS.
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(orbit.join("lock"))
        .context("open Orbit lock")?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow!("Orbit is active; stop the daemon before rebuilding"))?;

    let recipe = prior_recipe(&orbit)?;
    let generation = runtime::generation::Generation::derive(None, &recipe);
    let build = runtime::generation::Build::restart(&orbit, generation)
        .map_err(|error| anyhow!("begin generation {generation}: {error}"))?;

    let mechanics = mechanics::space::generation::build_prior(
        orbit.join("authority"),
        build.path(runtime::generation::Component::Mechanics),
    )
    .map_err(|error| anyhow!("build Mechanics generation: {error}"))?;

    let authority = Arc::new(SpaceAuthority::open_material(
        orbit.clone(),
        build.path(runtime::generation::Component::Mechanics),
        &space,
        device_seed,
    )?);
    let identity = runtime::Runtime::identity_from_seed(device_seed);
    let context = replica::transaction::CommitContext {
        space: &space,
        signer: &identity,
        authority_frontier: authority.current_frontier(),
    };
    let replica = replica::generation::build_prior(
        &orbit,
        build.path(runtime::generation::Component::Replica),
        &context,
        authority,
    )
    .map_err(|error| anyhow!("build Replica generation: {error}"))?;

    let evidence = combined_evidence(mechanics.evidence(), replica.evidence());
    let verification = build
        .verify(runtime::generation::Evidence::from_digest(evidence))
        .map_err(|error| anyhow!("seal generation {generation}: {error}"))?;
    let activation = verification
        .activate()
        .map_err(|error| anyhow!("activate generation {generation}: {error}"))?;

    Ok(Rebuild {
        generation: activation.generation(),
        effects: mechanics.effects(),
        bodies: replica.bodies(),
        receipts: replica.receipts(),
        evidence,
    })
}

fn prior_recipe(orbit: &Path) -> Result<Vec<u8>> {
    let mechanics = std::fs::read(orbit.join("authority/current-manifest"))
        .context("read prior Mechanics manifest")?;
    let replica =
        std::fs::read(orbit.join("current-manifest")).context("read prior Replica manifest")?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/orbit-generation/1/prior-journal-to-current");
    hash.update(b"journal/2");
    hash.update(&mechanics);
    hash.update(&replica);
    Ok(hash.finalize().as_bytes().to_vec())
}

fn combined_evidence(mechanics: [u8; 32], replica: [u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/orbit-generation/1/equivalence");
    hash.update(&mechanics);
    hash.update(&replica);
    *hash.finalize().as_bytes()
}
