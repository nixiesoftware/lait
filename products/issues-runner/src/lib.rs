//! The Issues World runner as a library: the service builder both backends
//! share, plus the wasm guest entry. The native binary (`main.rs`) serves this
//! service over a socket; a browser or the wasmtime host runs the wasm module
//! this crate compiles to.

use std::sync::Arc;

use world_sdk::WorldService;

/// Build the reviewed Issues service. `migrator` selects the pinned historical
/// implementation; every runner backend hands the same service to its transport.
pub fn build_service(
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

// On wasm32 the daemon or a browser runs this module in-process: no env, no
// args, no accept loop. The host hands the world id and version through
// `init`; the migrator variant is a native-only launch argument, so a wasm
// runner is the reviewed implementation.
#[cfg(target_arch = "wasm32")]
world_runner::export_world_runner!(|init: world_runner::wasm_abi::GuestInit| {
    let service = build_service(init.version, false)
        .expect("the Issues client package is embedded in this build");
    Arc::new(service) as Arc<dyn world_runner::Service>
});

/// Entropy from the host, so the guest imports only the ABI and `lait.random`
/// — no `crypto`, no wasm-bindgen. The custom backend of every getrandom major
/// the runner links routes here. Built under `--cfg getrandom_backend="custom"`
/// (which also cfg's out the wasm_js backend even where its feature is on).
#[cfg(target_arch = "wasm32")]
mod entropy {
    #[link(wasm_import_module = "lait")]
    extern "C" {
        /// Fill `len` bytes at `ptr` with host entropy.
        fn random(ptr: *mut u8, len: usize);
    }

    /// getrandom 0.3 and 0.4 both import this exact symbol.
    #[no_mangle]
    unsafe extern "Rust" fn __getrandom_v03_custom(
        dest: *mut u8,
        len: usize,
    ) -> Result<(), getrandom_v03::Error> {
        random(dest, len);
        Ok(())
    }

    fn fill_02(dest: &mut [u8]) -> Result<(), getrandom_v02::Error> {
        // SAFETY: `dest` is a valid, initialized slice for its length.
        unsafe { random(dest.as_mut_ptr(), dest.len()) };
        Ok(())
    }
    getrandom_v02::register_custom_getrandom!(fill_02);
}
