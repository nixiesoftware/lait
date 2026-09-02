//! The Issues World runner. Native, it is a supervised child process that
//! serves over a socket; on wasm32 it is a module the daemon runs in-process
//! through the same [`world_sdk::WorldService`]. Both build the identical
//! service — only the entry differs, because a wasm guest cannot block on an
//! accept loop.

use std::sync::Arc;

use world_sdk::WorldService;

/// Build the reviewed Issues service. `migrator` selects the pinned historical
/// implementation; every runner backend hands the same service to its transport.
fn build_service(
    version: String,
    migrator: bool,
) -> anyhow::Result<WorldService<issues::IssuesWorld>> {
    if migrator {
        let reviewed = issues::IssuesWorld::MIGRATOR_IMPLEMENTATION_ID;
        Ok(WorldService::new(issues::IssuesWorld::migrator(), reviewed)
            .with_exec(runtime::exec::Package::new().with_spec(issues::contract::verify_spec()))
            .with_handler(Arc::new(issues_app::IssuesCallHandler))
            .with_application(Arc::new(
                issues_app::application::IssuesApplication::default(),
            ))
            .with_client(issues_app::package()?))
    } else {
        let _ = version;
        let reviewed = issues_app::lifecycle::implementation_id();
        let spec = issues::contract::verify_spec();
        let build = issues::contract::verify_build(reviewed);
        let exec = runtime::exec::Package::new()
            .with_spec(spec)
            .with_build(build.clone())
            .with_handler(issues::handler::verify_handler(&build));
        Ok(WorldService::new(issues::IssuesWorld::new(), reviewed)
            .with_exec(exec)
            .with_handler(Arc::new(issues_app::IssuesCallHandler))
            .with_application(Arc::new(
                issues_app::application::IssuesApplication::default(),
            ))
            .with_client(issues_app::package()?))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    // What this World is called *here*. Symmetric with the version: both are
    // the host's facts and both fall back to what the tree compiled in.
    let world = issues::product_world();
    let version =
        std::env::var("LAIT_WORLD_VERSION").unwrap_or_else(|_| issues::RELEASE_VERSION.to_string());
    let migrator = std::env::args().any(|argument| argument == "--migrator");
    let service = build_service(version.clone(), migrator)?;
    world_runner::serve(world, version, service)
}

// On wasm32 the daemon runs this module in-process: no env, no args, no accept
// loop. The host hands the world id and version through `init`; the migrator
// variant is a native-only launch argument, so a wasm runner is the reviewed
// implementation. Execution under real limits is proven in a later slice; this
// wiring is what keeps the runner stack compiling for the browser target.
#[cfg(target_arch = "wasm32")]
world_runner::export_world_runner!(|init: world_runner::wasm_abi::GuestInit| {
    let service = build_service(init.version, false)
        .expect("the Issues client package is embedded in this build");
    Arc::new(service) as Arc<dyn world_runner::Service>
});

#[cfg(target_arch = "wasm32")]
fn main() {}
