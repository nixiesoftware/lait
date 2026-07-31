//! Host-side composition of Issues formation with the orbital lifecycle.
//!
//! Generic Space/Station ownership remains here. Product policy, initial
//! Catalog construction, and crash-resumable bootstrap persistence live in
//! `issues-app`.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use runtime::{ActivationOptions, Registry, Runtime};

use crate::orbital::{discover_space_id, orbital_store_root, unsupported_store_at, SpaceAuthority};

pub use issues_app::lifecycle::{BootstrapFault, BootstrapPhase, IssuesBootstrapRecord};

fn issues_registry() -> Result<Registry> {
    crate::world::packages()
        .build()
        .map(|(registry, _)| registry)
        .map_err(|error| anyhow::anyhow!("world registry: {error:?}"))
}

pub fn issues_implementation_id() -> [u8; 32] {
    issues_app::lifecycle::implementation_id()
}

/// Apply the product-supplied founder policy through the generic Mechanics
/// authority host.
pub fn seed_founder_policy(mechanics: &SpaceAuthority) -> Result<()> {
    let policy = issues_app::lifecycle::founder_policy();
    mechanics.activate_implementation(policy.world, policy.implementation)?;
    for grant in policy.grants {
        mechanics.grant_self_capability(grant.capability, grant.resource, grant.salt)?;
    }
    Ok(())
}

pub fn read_bootstrap_record(
    home: &Path,
    space: &crate::ids::SpaceId,
) -> Option<IssuesBootstrapRecord> {
    issues_app::lifecycle::read_bootstrap_record(&orbital_store_root(home), space)
}

pub fn form_space(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
) -> Result<(SpaceAuthority, runtime::SignedCoordinates)> {
    form_space_with_fault(home, device_seed, display_name, None, None)
}

/// Form or resume the generic orbital footprint, then hand the docked Session
/// to the Issues package for its product bootstrap.
#[doc(hidden)]
pub fn form_space_with_fault(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
    project: Option<(String, String)>,
    fault: Option<BootstrapFault>,
) -> Result<(SpaceAuthority, runtime::SignedCoordinates)> {
    if let Some(error) = unsupported_store_at(home) {
        return Err(anyhow::anyhow!("{error}"));
    }
    let root = orbital_store_root(home);
    let (mechanics, coordinates) = match discover_space_id(home) {
        Some(space) => {
            let mechanics = SpaceAuthority::open(&root, &space, device_seed)?;
            let coordinates =
                mechanics.mint_coordinates(device_seed, display_name, vec![], None)?;
            (mechanics, coordinates)
        }
        None => SpaceAuthority::form(&root, device_seed, display_name, vec![])?,
    };

    seed_founder_policy(&mechanics)?;

    let runtime = Runtime::open(
        root.clone(),
        issues_registry()?,
        Arc::new(mechanics.clone()),
        Arc::new(mechanics.clone()),
    );
    let orbit = runtime
        .materialize(&coordinates)
        .map_err(|error| anyhow::anyhow!("materialize orbit: {error:?}"))?;
    let station = orbit
        .open(ActivationOptions::offline())
        .map_err(|error| anyhow::anyhow!("activate: {error:?}"))?;
    let identity = Runtime::identity_from_seed(device_seed);
    let session = station
        .dock(&crate::world::contract::world_id(), &identity)
        .map_err(|error| anyhow::anyhow!("dock: {error:?}"))?;
    let initial_project =
        project.map(|(name, key)| issues_app::lifecycle::InitialProject { name, key });
    let bootstrap = issues_app::lifecycle::bootstrap_tracker(
        &root,
        &mechanics.space(),
        &session,
        &identity,
        crate::crypto::device_from_seed(device_seed).as_str(),
        display_name,
        initial_project,
        fault,
    );
    let _ = station.vacate();
    bootstrap?;
    Ok((mechanics, coordinates))
}

pub fn found_space_cli(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
) -> Result<(crate::ids::SpaceId, crate::orbits::ProjectBrief)> {
    let project = issues_app::lifecycle::InitialProject::for_space(display_name);
    let (mechanics, _) = form_space_with_fault(
        home,
        device_seed,
        display_name,
        Some((project.name.clone(), project.key.clone())),
        None,
    )?;
    Ok((
        mechanics.space(),
        crate::orbits::ProjectBrief {
            key: project.key,
            name: project.name,
        },
    ))
}

/// Enter and materialize an Orbit. No Issues bootstrap runs for a joiner; its
/// product state arrives through convergence after admission.
pub fn enter_space(
    home: &Path,
    device_seed: &[u8; 32],
    invite_link: &str,
) -> Result<(SpaceAuthority, runtime::SignedCoordinates)> {
    if let Some(error) = unsupported_store_at(home) {
        return Err(anyhow::anyhow!("{error}"));
    }
    let coordinates = runtime::SignedCoordinates::parse_link(invite_link.trim())
        .map_err(|error| anyhow::anyhow!("invalid invite link: {error}"))?;
    let root = orbital_store_root(home);
    let mechanics = SpaceAuthority::enter(&root, device_seed, &coordinates)?;
    let runtime = Runtime::open(
        root,
        issues_registry()?,
        Arc::new(mechanics.clone()),
        Arc::new(mechanics.clone()),
    );
    runtime
        .materialize(&coordinates)
        .map_err(|error| anyhow::anyhow!("materialize orbit: {error:?}"))?;
    Ok((mechanics, coordinates))
}
