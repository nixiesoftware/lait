//! Explicit in-process World fixtures for substrate integration tests.
//!
//! Production never links these packages; end-to-end runner coverage uses
//! staged releases. These fixtures keep authority/transport tests focused and
//! make their product dependency visible at the test call site.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn packages() -> lait::orbital::WorldPackages {
    let implementation = issues_app::lifecycle::implementation_id();
    let verify = issues::contract::verify_build(implementation);
    let issues =
        lait::orbital::WorldPackage::new(Arc::new(issues::IssuesWorld::new()), implementation)
            .with_control(Arc::new(issues_app::IssuesCallHandler))
            .with_exec(
                runtime::exec::Package::new()
                    .with_spec(issues::contract::verify_spec())
                    .with_build(verify.clone())
                    .with_handler(issues::handler::verify_handler(&verify)),
            )
            .with_projector(Arc::new(
                issues_app::application::IssuesApplication::default(),
            ))
            .with_lifecycle(Arc::new(
                issues_app::application::IssuesApplication::default(),
            ));

    let migrator_world = issues::IssuesWorld::migrator();
    let migrator_id = issues::IssuesWorld::MIGRATOR_IMPLEMENTATION_ID;
    let migrator = lait::orbital::WorldPackage::new(Arc::new(migrator_world), migrator_id)
        .with_control(Arc::new(issues_app::IssuesCallHandler))
        .with_exec(runtime::exec::Package::new().with_spec(issues::contract::verify_spec()))
        .with_projector(Arc::new(
            issues_app::application::IssuesApplication::default(),
        ))
        .with_lifecycle(Arc::new(
            issues_app::application::IssuesApplication::default(),
        ))
        .historical();

    let signage = lait::orbital::WorldPackage::new(
        Arc::new(signage::SignageWorld::new()),
        signage_app::implementation_id(),
    )
    .with_control(Arc::new(signage_app::SignageCallHandler))
    .with_projector(Arc::new(signage_app::application::SignageApplication))
    .with_lifecycle(Arc::new(signage_app::application::SignageApplication));

    lait::orbital::WorldPackages::new()
        .with_package(issues)
        .with_package(migrator)
        .with_package(signage)
}

pub fn clients() -> world_interface::WorldClientRegistry {
    world_interface::WorldClientRegistry::new()
        .with_package(issues_app::package().expect("Issues test client package"))
        .and_then(|registry| {
            registry.with_package(signage_app::package().expect("Signage test client package"))
        })
        .expect("test World client registry")
}

pub fn form_space(
    home: &Path,
    seed: &[u8; 32],
    name: &str,
) -> anyhow::Result<(
    lait::orbital::SpaceAuthority,
    runtime::coordinates::SignedCoordinates,
)> {
    lait::orbital::form_space(&packages(), home, seed, name)
}

pub fn found_space(
    home: &Path,
    seed: &[u8; 32],
    name: &str,
) -> anyhow::Result<(mechanics::ids::SpaceId, lait::orbits::ProjectBrief)> {
    lait::orbital::found_space(&packages(), home, seed, name)
}

pub fn enter_space(
    home: &Path,
    seed: &[u8; 32],
    link: &str,
) -> anyhow::Result<(
    lait::orbital::SpaceAuthority,
    runtime::coordinates::SignedCoordinates,
)> {
    lait::orbital::enter_space(&packages(), home, seed, link)
}

pub fn seed_founder_policy(mechanics: &lait::orbital::SpaceAuthority) -> anyhow::Result<()> {
    lait::orbital::seed_founder_policy(mechanics, &packages())
}

pub fn role_evidence(
    role: &str,
    parent_manifest_root: [u8; 32],
) -> mechanics::authorization::WorldAssignmentEvidence {
    let role_id = issues::roles::resolve_role_selector(role).expect("known Issues test role");
    let revision = issues::roles::built_in(role_id).expect("built-in Issues test role");
    issues::roles::role_admission_evidence(&revision, parent_manifest_root)
}

pub async fn run_station_process(
    home: PathBuf,
    factory: &dyn comms::TransportFactory,
) -> anyhow::Result<()> {
    lait::orbital::run_station_process(home, factory, packages()).await
}

pub async fn run_station_process_with(
    home: PathBuf,
    seed: [u8; 32],
    factory: &dyn comms::TransportFactory,
) -> anyhow::Result<()> {
    lait::orbital::run_station_process_with(home, seed, factory, packages()).await
}
