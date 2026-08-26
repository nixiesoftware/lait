fn main() -> anyhow::Result<()> {
    // What this World is called *here*. Symmetric with the version below: both
    // are the host's facts, both fall back to what the tree compiled in, and
    // neither is something a World gets to insist on. `product_world` resolves
    // it once from the launcher's environment.
    let world = issues::product_world();
    let version =
        std::env::var("LAIT_WORLD_VERSION").unwrap_or_else(|_| issues::RELEASE_VERSION.to_string());
    let migrator = std::env::args().any(|argument| argument == "--migrator");
    if migrator {
        let reviewed = issues::IssuesWorld::MIGRATOR_IMPLEMENTATION_ID;
        world_runner::serve(
            world,
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
            world,
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
