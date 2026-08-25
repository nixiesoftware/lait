//! Explicit in-process World fixtures for root unit tests.
//!
//! This module is compiled only under `cfg(test)`. Production discovers and
//! launches immutable releases through [`super::installed`].

use std::sync::Arc;

pub use issues::{contract, IssuesWorld};

pub const ISSUES_ID: &str = "com.lait.issues";
pub const ISSUES_MOUNT: &str = "issues";

pub fn packages() -> crate::orbital::WorldPackages {
    let implementation = issues_app::lifecycle::implementation_id();
    let verify = issues::contract::verify_build(implementation);
    let issues =
        crate::orbital::WorldPackage::new(Arc::new(issues::IssuesWorld::new()), implementation)
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
    let migrator = crate::orbital::WorldPackage::new(Arc::new(migrator_world), migrator_id)
        .with_control(Arc::new(issues_app::IssuesCallHandler))
        .with_exec(runtime::exec::Package::new().with_spec(issues::contract::verify_spec()))
        .with_projector(Arc::new(
            issues_app::application::IssuesApplication::default(),
        ))
        .with_lifecycle(Arc::new(
            issues_app::application::IssuesApplication::default(),
        ))
        .historical();

    let signage = crate::orbital::WorldPackage::new(
        Arc::new(signage::SignageWorld::new()),
        signage_app::implementation_id(),
    )
    .with_control(Arc::new(signage_app::SignageCallHandler))
    .with_projector(Arc::new(signage_app::application::SignageApplication))
    .with_lifecycle(Arc::new(signage_app::application::SignageApplication));

    crate::orbital::WorldPackages::new()
        .with_package(issues)
        .with_package(migrator)
        .with_package(signage)
}

pub fn client_packages() -> world_interface::WorldClientRegistry {
    world_interface::WorldClientRegistry::new()
        .with_package(issues_app::package().expect("Issues test client package"))
        .and_then(|registry| {
            registry.with_package(signage_app::package().expect("Signage test client package"))
        })
        .expect("test World client registry")
}
