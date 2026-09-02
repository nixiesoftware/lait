//! Compile the proof-World to wasm32 so the integration test can run it under
//! `cargo nextest` with no external harness. The proof crate is its own
//! workspace and links only `world-runner`, so this needs the wasm32 target
//! but no wasm C toolchain. If the target is not installed, the build emits a
//! cfg the test reads to skip honestly rather than to pass.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=tests/proof-world/src/lib.rs");
    println!("cargo:rerun-if-changed=tests/proof-world/Cargo.toml");
    println!("cargo:rustc-check-cfg=cfg(proof_world_wasm)");

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proof = manifest.join("tests/proof-world");
    // A dedicated target dir under OUT_DIR keeps the guest build off the host
    // workspace's lock and out of its target tree.
    let target_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("proof-world-target");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .current_dir(&proof)
        .env("CARGO_TARGET_DIR", &target_dir)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .status();

    let wasm = target_dir.join("wasm32-unknown-unknown/release/proof_world.wasm");
    match status {
        Ok(status) if status.success() && wasm.is_file() => {
            println!("cargo:rustc-cfg=proof_world_wasm");
            println!("cargo:rustc-env=PROOF_WORLD_WASM={}", wasm.display());
        }
        _ => {
            // The wasm32 target is absent or the guest did not build; the test
            // reads the missing cfg and reports "could not be asked".
            println!(
                "cargo:warning=proof-world was not built for wasm32 (is the target installed?); the wasm-runner integration test will skip"
            );
        }
    }
}
