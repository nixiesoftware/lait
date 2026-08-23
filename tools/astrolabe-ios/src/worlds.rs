//! The iOS platform adapter for reviewed first-party Worlds.
//!
//! Apple platforms do not permit an application to spawn helper executables or
//! install new native code after signing. The independently distributed native
//! runner is therefore a desktop/server boundary. iOS keeps the same generic
//! Runtime and client interfaces, but the signed application links only Lait's
//! reviewed first-party implementations. This exception lives here, outside the
//! product-blind host crate, and must not become a fallback on process-capable
//! platforms.

use std::sync::Arc;

use anyhow::Result;

pub(crate) struct Installation {
    pub packages: lait::orbital::WorldPackages,
    pub clients: world_interface::WorldClientRegistry,
}

pub(crate) fn installation() -> Result<Installation> {
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
            ))
            .with_release_version(issues::RELEASE_VERSION);

    let migrator_id = issues::IssuesWorld::migrator_implementation_descriptor()
        .id()
        .map_err(anyhow::Error::msg)?;
    let migrator =
        lait::orbital::WorldPackage::new(Arc::new(issues::IssuesWorld::migrator()), migrator_id)
            .with_control(Arc::new(issues_app::IssuesCallHandler))
            .with_exec(runtime::exec::Package::new().with_spec(issues::contract::verify_spec()))
            .with_projector(Arc::new(
                issues_app::application::IssuesApplication::default(),
            ))
            .with_lifecycle(Arc::new(
                issues_app::application::IssuesApplication::default(),
            ))
            .with_release_version(issues::RELEASE_VERSION)
            .historical();

    let signage = lait::orbital::WorldPackage::new(
        Arc::new(signage::SignageWorld::new()),
        signage_app::implementation_id(),
    )
    .with_control(Arc::new(signage_app::SignageCallHandler))
    .with_projector(Arc::new(signage_app::application::SignageApplication))
    .with_lifecycle(Arc::new(signage_app::application::SignageApplication))
    .with_release_version(signage::RELEASE_VERSION);

    let packages = lait::orbital::WorldPackages::new()
        .with_package(issues)
        .with_package(migrator)
        .with_package(signage);
    let clients = client_packages()?;
    Ok(Installation { packages, clients })
}

pub(crate) fn client_packages() -> Result<world_interface::WorldClientRegistry> {
    world_interface::WorldClientRegistry::new()
        .with_package(issues_app::package()?)
        .and_then(|registry| registry.with_package(signage_app::package()?))
        .map_err(anyhow::Error::msg)
}

pub(crate) fn primary_mount() -> &'static str {
    issues_app::MOUNT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_adapter_carries_exactly_the_reviewed_first_party_defaults() {
        let installed = installation().expect("reviewed iOS World adapter");
        let issues_world = issues::contract::world_id();
        let signage_world = signage::contract::world_id();

        let mut worlds = installed
            .packages
            .world_ids()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        worlds.sort();
        assert_eq!(
            worlds,
            vec![
                issues::PRODUCT_WORLD.to_string(),
                signage::PRODUCT_WORLD.to_string()
            ]
        );
        assert_eq!(
            installed.packages.release_version(&issues_world),
            Some(issues::RELEASE_VERSION)
        );
        assert_eq!(
            installed.packages.release_version(&signage_world),
            Some(signage::RELEASE_VERSION)
        );
        assert_eq!(
            installed
                .clients
                .package_for_mount(issues_app::MOUNT)
                .map(|package| package.world()),
            Some(&issues_world)
        );
        assert_eq!(
            installed
                .clients
                .package_for_mount(signage_app::MOUNT)
                .map(|package| package.world()),
            Some(&signage_world)
        );
    }
}
