fn main() -> anyhow::Result<()> {
    let version = std::env::var("LAIT_WORLD_VERSION")
        .unwrap_or_else(|_| signage::RELEASE_VERSION.to_string());
    let reviewed = signage_app::implementation_id();
    world_runner::serve(
        signage::contract::world_id().to_string(),
        version,
        world_sdk::WorldService::new(signage::SignageWorld::new(), reviewed)
            .with_handler(std::sync::Arc::new(signage_app::SignageCallHandler))
            .with_application(std::sync::Arc::new(
                signage_app::application::SignageApplication,
            ))
            .with_client(signage_app::package()?),
    )
}
