//! Host-side composition of World packages with the orbital lifecycle.
//!
//! Generic Space/Station ownership remains here. Each installed World package
//! supplies its own founder policy, initial scopes, and crash-resumable
//! bootstrap hook through [`WorldPackages`].

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use runtime::{plane::Activation, world::Catalog, Runtime};

use crate::orbital::{
    discover_space, orbital_store_root, unsupported_store_at, BootstrapContext, InitialScope,
    SpaceAuthority, SpaceStore, WorldPackages,
};

/// A home bound to more than one Space. Resuming formation there would pick one
/// arbitrarily, and forming beside them would add a third.
fn ambiguous_home(home: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "{} holds more than one orbital Space; a home binds one",
        home.display()
    )
}

fn world_registry(packages: &WorldPackages) -> Result<Catalog> {
    packages
        .build()
        .map(|(registry, _)| registry)
        .map_err(|error| anyhow::anyhow!("world registry: {error:?}"))
}

/// Apply the product-supplied founder policy through the generic Mechanics
/// authority host.
pub fn seed_founder_policy(mechanics: &SpaceAuthority) -> Result<()> {
    seed_founder_policies(mechanics, &crate::world::packages())
}

fn seed_founder_policies(mechanics: &SpaceAuthority, packages: &WorldPackages) -> Result<()> {
    for (world, implementation, version, grants) in packages.founder_policies()? {
        mechanics.activate_implementation(world.as_str(), implementation, version)?;
        for grant in grants {
            mechanics.grant_self_capability(grant.capability, grant.resource, grant.salt)?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub fn read_bootstrap_record(
    home: &Path,
    space: &mechanics::ids::SpaceId,
) -> Option<issues_app::lifecycle::IssuesBootstrapRecord> {
    issues_app::lifecycle::read_bootstrap_record(&orbital_store_root(home), space)
}

pub fn form_space(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
) -> Result<(SpaceAuthority, runtime::coordinates::SignedCoordinates)> {
    form_space_with_scopes(home, device_seed, display_name, Vec::new())
}

/// Form or resume the generic orbital footprint, then hand each package its
/// own docked Session for product bootstrap.
fn form_space_with_scopes(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
    initial_scopes: Vec<(replica::body::WorldId, InitialScope)>,
) -> Result<(SpaceAuthority, runtime::coordinates::SignedCoordinates)> {
    if let Some(error) = unsupported_store_at(home) {
        return Err(anyhow::anyhow!("{error}"));
    }
    let root = orbital_store_root(home);
    let (mechanics, coordinates) = match discover_space(home) {
        SpaceStore::One(space) => {
            let mechanics = SpaceAuthority::open(&root, &space, device_seed)?;
            let coordinates =
                mechanics.mint_coordinates(device_seed, display_name, vec![], None)?;
            (mechanics, coordinates)
        }
        SpaceStore::Absent => SpaceAuthority::form(&root, device_seed, display_name, vec![])?,
        SpaceStore::Several => return Err(ambiguous_home(home)),
    };

    let packages = crate::world::packages();
    seed_founder_policies(&mechanics, &packages)?;

    let runtime = Runtime::open(
        root.clone(),
        world_registry(&packages)?,
        Arc::new(mechanics.clone()),
        Arc::new(mechanics.clone()),
    );
    let orbit = runtime
        .materialize(&coordinates)
        .map_err(|error| anyhow::anyhow!("materialize orbit: {error:?}"))?;
    let station = orbit
        .open(Activation::offline())
        .map_err(|error| anyhow::anyhow!("activate: {error:?}"))?;
    let identity = Runtime::identity_from_seed(device_seed);
    let bootstrap = packages
        .lifecycle_world_ids()
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .try_for_each(|world| {
            let session = station
                .dock(&world, &identity)
                .map_err(|error| anyhow::anyhow!("dock {world}: {error:?}"))?;
            let initial_scope = initial_scopes
                .iter()
                .find(|(candidate, _)| candidate == &world)
                .map(|(_, scope)| scope);
            packages.bootstrap(
                &world,
                BootstrapContext {
                    store_root: &root,
                    space: &mechanics.space(),
                    session: &session,
                    identity: &identity,
                    device: mechanics::actor::device_from_seed(device_seed).as_str(),
                    display_name,
                    initial_scope,
                },
            )
        });
    let _ = station.vacate();
    bootstrap?;
    Ok((mechanics, coordinates))
}

#[cfg(test)]
fn form_space_with_fault(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
    fault: issues_app::lifecycle::Fault,
) -> Result<(SpaceAuthority, runtime::coordinates::SignedCoordinates)> {
    if let Some(error) = unsupported_store_at(home) {
        return Err(anyhow::anyhow!("{error}"));
    }
    let root = orbital_store_root(home);
    let (mechanics, coordinates) = match discover_space(home) {
        SpaceStore::One(space) => {
            let mechanics = SpaceAuthority::open(&root, &space, device_seed)?;
            let coordinates =
                mechanics.mint_coordinates(device_seed, display_name, vec![], None)?;
            (mechanics, coordinates)
        }
        SpaceStore::Absent => SpaceAuthority::form(&root, device_seed, display_name, vec![])?,
        SpaceStore::Several => return Err(ambiguous_home(home)),
    };

    seed_founder_policy(&mechanics)?;
    let runtime = Runtime::open(
        root.clone(),
        world_registry(&crate::world::packages())?,
        Arc::new(mechanics.clone()),
        Arc::new(mechanics.clone()),
    );
    let orbit = runtime
        .materialize(&coordinates)
        .map_err(|error| anyhow::anyhow!("materialize orbit: {error:?}"))?;
    let station = orbit
        .open(Activation::offline())
        .map_err(|error| anyhow::anyhow!("activate: {error:?}"))?;
    let identity = Runtime::identity_from_seed(device_seed);
    let session = station
        .dock(&crate::world::contract::world_id(), &identity)
        .map_err(|error| anyhow::anyhow!("dock: {error:?}"))?;
    let bootstrap = issues_app::lifecycle::bootstrap_tracker_with_fault(
        &root,
        &mechanics.space(),
        &session,
        &identity,
        mechanics::actor::device_from_seed(device_seed).as_str(),
        display_name,
        None,
        fault,
    );
    let _ = station.vacate();
    bootstrap?;
    Ok((mechanics, coordinates))
}

pub fn found_space(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
) -> Result<(mechanics::ids::SpaceId, crate::orbits::ProjectBrief)> {
    let initial_scopes = crate::world::packages().initial_scopes(display_name);
    let primary = initial_scopes
        .first()
        .map(|(_, scope)| scope.clone())
        .ok_or_else(|| anyhow::anyhow!("no bundled World declares an initial scope"))?;
    let (mechanics, _) = form_space_with_scopes(home, device_seed, display_name, initial_scopes)?;
    Ok((
        mechanics.space(),
        crate::orbits::ProjectBrief {
            key: primary.key,
            name: primary.name,
        },
    ))
}

/// Enter and materialize an Orbit. No Issues bootstrap runs for a joiner; its
/// product state arrives through convergence after admission.
pub fn enter_space(
    home: &Path,
    device_seed: &[u8; 32],
    invite_link: &str,
) -> Result<(SpaceAuthority, runtime::coordinates::SignedCoordinates)> {
    if let Some(error) = unsupported_store_at(home) {
        return Err(anyhow::anyhow!("{error}"));
    }
    let coordinates = runtime::coordinates::SignedCoordinates::parse_link(invite_link.trim())
        .map_err(|error| anyhow::anyhow!("invalid invite link: {error}"))?;
    let root = orbital_store_root(home);
    let mechanics = SpaceAuthority::enter(&root, device_seed, &coordinates)?;
    let runtime = Runtime::open(
        root,
        world_registry(&crate::world::packages())?,
        Arc::new(mechanics.clone()),
        Arc::new(mechanics.clone()),
    );
    runtime
        .materialize(&coordinates)
        .map_err(|error| anyhow::anyhow!("materialize orbit: {error:?}"))?;
    Ok((mechanics, coordinates))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use issues_app::lifecycle::{BootstrapPhase, Fault};

    use super::{form_space, form_space_with_fault, read_bootstrap_record};
    use crate::orbital::{orbital_store_root, SpaceAuthority};
    use crate::world::contract;

    const FOUNDER_SEED: [u8; 32] = [71u8; 32];
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_home() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("lait-catalog-fault-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temporary home");
        dir
    }

    fn snapshot_projects(home: &Path, mechanics: &SpaceAuthority) -> usize {
        let runtime = runtime::Runtime::open(
            orbital_store_root(home),
            runtime::world::Builder::new()
                .register(Arc::new(crate::world::IssuesWorld::new()))
                .build()
                .expect("Issues registry"),
            Arc::new(mechanics.clone()),
            Arc::new(mechanics.clone()),
        );
        let station = runtime
            .acquire(&mechanics.space())
            .expect("Orbit")
            .open(runtime::plane::Activation::offline())
            .expect("Station");
        let identity = runtime::Runtime::identity_from_seed(&FOUNDER_SEED);
        let session = station
            .dock(&contract::world_id(), &identity)
            .expect("Session");
        let projection = session
            .query(runtime::world::Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: contract::IssueQuery::Snapshot.to_json(),
                publication: None,
            })
            .expect("snapshot");
        let value: serde_json::Value =
            serde_json::from_slice(&projection.bytes).expect("snapshot JSON");
        let count = value["catalog"]["projects"]
            .as_object()
            .map_or(0, serde_json::Map::len);
        let _ = station.vacate();
        count
    }

    #[test]
    fn formation_resumes_exact_signed_action_after_each_private_fault() {
        for fault in [
            Fault::BeforeRecord,
            Fault::AfterRecord,
            Fault::BeforeSubmit,
            Fault::BeforeComplete,
        ] {
            let home = temp_home();
            assert!(
                form_space_with_fault(&home, &FOUNDER_SEED, "Fault Space", fault).is_err(),
                "fault interrupts formation"
            );

            let space = crate::orbital::discover_space(&home)
                .single()
                .expect("Space store");
            let interrupted = read_bootstrap_record(&home, &space);
            match fault {
                Fault::BeforeRecord => assert!(interrupted.is_none()),
                _ => {
                    let record = interrupted.clone().expect("durable bootstrap record");
                    assert_eq!(record.phase, BootstrapPhase::Recorded);
                    assert_eq!(record.space, space.as_str());
                }
            }

            let (mechanics, _) = form_space(&home, &FOUNDER_SEED, "Fault Space").expect("resume");
            let complete = read_bootstrap_record(&home, &space).expect("complete record");
            assert_eq!(complete.phase, BootstrapPhase::Complete);
            if let Some(record) = interrupted {
                assert_eq!(record.signed_action, complete.signed_action);
                assert_eq!(record.request_id, complete.request_id);
                assert_eq!(
                    record.canonical_intent_bytes,
                    complete.canonical_intent_bytes
                );
            }
            assert_eq!(snapshot_projects(&home, &mechanics), 1);
            let (reopened, _) =
                form_space(&home, &FOUNDER_SEED, "Fault Space").expect("idempotent resume");
            assert_eq!(snapshot_projects(&home, &reopened), 1);
            let _ = std::fs::remove_dir_all(home);
        }
    }
}
