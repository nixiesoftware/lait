//! The Issues World runner binary: a supervised child process that serves the
//! reviewed Issues service over a socket. The service and the wasm guest entry
//! live in the library (`lib.rs`) so both backends build the identical service.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    // What this World is called *here*. Symmetric with the version: both are
    // the host's facts and both fall back to what the tree compiled in.
    let world = issues::product_world();
    let version =
        std::env::var("LAIT_WORLD_VERSION").unwrap_or_else(|_| issues::RELEASE_VERSION.to_string());
    let migrator = std::env::args().any(|argument| argument == "--migrator");
    let service = lait_issues_runner::build_service(version.clone(), migrator)?;
    world_runner::serve(world, version, service)
}

#[cfg(target_arch = "wasm32")]
fn main() {}
