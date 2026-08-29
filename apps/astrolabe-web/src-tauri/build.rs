fn main() {
    // `tauri.conf.json` bundles `world-catalog/` as a resource, and the catalog
    // is staged by an npm script rather than committed — so on a fresh clone
    // the first `cargo build` in this directory fails inside the bundler with
    // `resource path "world-catalog" doesn't exist`. That names the symptom and
    // nothing you could do about it, and the thing you could do about it lives
    // in a different language in a different directory.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest directory");
    let catalog = std::path::Path::new(&manifest).join("world-catalog");
    if !catalog.is_dir() {
        panic!(
            "the first-party World catalog is not staged, so this host cannot be built.\n\
             \n\
             Run this once, in apps/astrolabe-web:\n\
             \n\
             \x20   npm run stage-sidecar\n\
             \n\
             It builds the lait sidecar and stages the catalog beside it. \
             `npm run tauri dev` and `npm run tauri build` both run it for you; \
             a bare `cargo` invocation here does not.\n\
             \n\
             (expected a directory at {})",
            catalog.display()
        );
    }
    tauri_build::build()
}
