fn main() -> anyhow::Result<()> {
    let version =
        std::env::var("LAIT_WORLD_VERSION").unwrap_or_else(|_| issues::RELEASE_VERSION.to_string());
    let migrator = std::env::args().any(|argument| argument == "--migrator");
    if migrator {
        let implementation = issues::IssuesWorld::migrator_implementation_descriptor();
        let reviewed = implementation
            .id()
            .map_err(|error| anyhow::anyhow!("invalid Issues migrator descriptor: {error}"))?;
        world_runner::serve(
            issues::PRODUCT_WORLD,
            version,
            world_sdk::WorldService::new(issues::IssuesWorld::migrator(), reviewed)
                .with_exec(runtime::exec::Package::new().with_spec(issues::contract::verify_spec()))
                .with_handler(std::sync::Arc::new(issues_app::IssuesCallHandler))
                .with_application(std::sync::Arc::new(
                    issues_app::application::IssuesApplication::default(),
                ))
                .with_client(issues_app::package()?),
        )
    } else {
        let reviewed = issues_app::lifecycle::implementation_id();
        let spec = issues::contract::verify_spec();
        let build = issues::contract::verify_build(reviewed);
        let exec = runtime::exec::Package::new()
            .with_spec(spec)
            .with_build(build.clone())
            .with_handler(issues::handler::verify_handler(&build));
        world_runner::serve(
            issues::PRODUCT_WORLD,
            version,
            world_sdk::WorldService::new(issues::IssuesWorld::new(), reviewed)
                .with_exec(exec)
                .with_handler(std::sync::Arc::new(issues_app::IssuesCallHandler))
                .with_application(std::sync::Arc::new(
                    issues_app::application::IssuesApplication::default(),
                ))
                .with_client(issues_app::package()?),
        )
    }
}
