//! Application composition of an Orbit representation rebuild.

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use runtime::world::AuthorityView;

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

/// What a ledger migration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerMigration {
    /// False when the ledger was already current and nothing was written.
    pub migrated: bool,
    pub effects: u64,
    /// Where the outgoing ledger was kept, when one was replaced.
    pub previous: Option<std::path::PathBuf>,
}

/// The ledger a migration keeps the outgoing generation at.
const PREVIOUS_LEDGER_DIR: &str = "authority.previous";
/// Where a migration builds before it swaps. Beside the ledger, so the swap is
/// a rename within one directory.
const STAGING_LEDGER_DIR: &str = "authority.staged";

/// Migrate an authority ledger written by a prior journal generation into the
/// current one, leaving the Replica beside it untouched.
///
/// The journal refuses to upgrade at open by design, so the upgrade happens
/// here: the prior store is read through the migration reader, rebuilt and
/// verified at a staging path, and only then swapped in. The outgoing ledger
/// is kept rather than deleted, and a rebuilt ledger that will not open is
/// rolled back.
///
/// Distinct from [`rebuild_prior`], which rebuilds every component into a new
/// generation. A Replica already at a readable format needs no rebuild, and
/// rebuilding it would be a second representation of data that is already
/// current.
pub fn migrate_prior_ledger(home: &Path) -> Result<LedgerMigration> {
    let space = match discover_space(home) {
        SpaceStore::One(space) => space,
        SpaceStore::Absent => return Err(anyhow!("no orbital Space in this home")),
        SpaceStore::Several => {
            return Err(anyhow!(
                "this home holds more than one orbital Space; a migration has no way to pick one"
            ))
        }
    };
    let orbit = orbital_store_root(home).join(space.as_str());
    let ledger = orbit.join("authority");

    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(orbit.join("lock"))
        .context("open Orbit lock")?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow!("Orbit is active; stop the daemon before migrating"))?;

    if mechanics::space::Authority::open(&ledger).is_ok() {
        return Ok(LedgerMigration {
            migrated: false,
            effects: 0,
            previous: None,
        });
    }

    let staged = orbit.join(STAGING_LEDGER_DIR);
    let previous = orbit.join(PREVIOUS_LEDGER_DIR);
    if previous.exists() {
        return Err(anyhow!(
            "{} already holds a kept ledger; move it aside before migrating again",
            previous.display()
        ));
    }
    let _ = std::fs::remove_dir_all(&staged);
    std::fs::create_dir_all(&staged).context("create the staging ledger root")?;

    let verification = mechanics::space::generation::build_prior(&ledger, &staged)
        .map_err(|error| anyhow!("rebuild the prior ledger: {error}"))?;

    std::fs::rename(&ledger, &previous).context("keep the outgoing ledger")?;
    if let Err(error) = std::fs::rename(&staged, &ledger) {
        let _ = std::fs::rename(&previous, &ledger);
        return Err(anyhow!("install the rebuilt ledger: {error}"));
    }
    if let Err(error) = mechanics::space::Authority::open(&ledger) {
        let _ = std::fs::rename(&ledger, &staged);
        let _ = std::fs::rename(&previous, &ledger);
        return Err(anyhow!("the rebuilt ledger does not open: {error}"));
    }

    Ok(LedgerMigration {
        migrated: true,
        effects: verification.effects(),
        previous: Some(previous),
    })
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
    let replica_target = build.path(runtime::generation::Component::Replica);
    let needs_semantic_migration =
        replica::generation::PriorReplicaSource::open(&orbit, authority.clone())
            .is_ok_and(|source| source.body_count() != 0);
    let replica = if needs_semantic_migration {
        let actor = authority
            .my_actor()
            .ok_or_else(|| anyhow!("this device has no actor standing for semantic migration"))?;
        let device = identity.device().clone();
        replica::generation::migrate_prior(
            &orbit,
            &replica_target,
            &context,
            authority.clone(),
            authority.as_ref(),
            &actor,
            &device,
            |world| {
                mechanics::authorization::AuthorizationDemand::require(
                    mechanics::authorization::PolicyCapability::new(world.as_str(), "space.admin"),
                    mechanics::authorization::Resource::root(world.as_str()),
                )
                .encode_canonical()
                .map_err(|_| {
                    replica::transaction::commit::Failure::Integrity(
                        replica::transaction::commit::Defect::Encoding,
                    )
                })
            },
            |world, core| {
                let implementation = authority
                    .active_implementation(world, &context.authority_frontier)
                    .map_err(|_| {
                        mechanics::authorization::Refusal::Denied(
                            mechanics::authorization::DenialReason::Internal(
                                "active implementation could not be resolved for migration",
                            ),
                        )
                    })?
                    .ok_or(mechanics::authorization::Refusal::Denied(
                        mechanics::authorization::DenialReason::Internal(
                            "the prior World's implementation is not active",
                        ),
                    ))?;
                authority.authorize_mutation(
                    &space,
                    world,
                    &actor,
                    &device,
                    &context.authority_frontier,
                    core.parent_manifest_root,
                    implementation,
                    core.intent_digest,
                    &core.demand,
                    core.operations_digest,
                    core.digest(),
                )
            },
        )
        .map_err(|error| anyhow!("migrate prior Replica facts: {error}"))?
    } else {
        replica::generation::build_prior(&orbit, &replica_target, &context, authority.clone())
            .map_err(|error| anyhow!("build Replica generation: {error}"))?
    };

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

#[cfg(test)]
mod real_data_diagnostic {
    #[test]
    #[ignore = "requires LAIT_REAL_REBUILD_HOME pointing at a disposable copied Space home"]
    fn copied_prior_space_reports_the_exact_rebuild_failure() {
        let home = std::env::var_os("LAIT_REAL_REBUILD_HOME")
            .map(std::path::PathBuf::from)
            .expect("LAIT_REAL_REBUILD_HOME");
        let seed = crate::config::load_or_create_identity(&home).expect("identity seed");
        let rebuilt = super::rebuild_prior(&home, &seed);
        eprintln!("REAL_REBUILD_RESULT={rebuilt:?}");
        rebuilt.expect("copied real Space rebuild");
    }
}
