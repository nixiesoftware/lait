//! Compile the proof-World to wasm32 so the browser runner test can drive a
//! real guest module. Reuses the same fixture the native wasmtime host proves
//! against (`crates/world-runner-wasm/tests/proof-world`), so both backends
//! exercise one guest. Only built when the `probe-runner` feature is on and
//! the wasm32 target is installed; otherwise the test reads the missing cfg
//! and does not run.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(proof_world_wasm)");
    if std::env::var("CARGO_FEATURE_PROBE_RUNNER").is_err() {
        return;
    }

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proof = manifest.join("../crates/world-runner-wasm/tests/proof-world");
    println!("cargo:rerun-if-changed={}", proof.join("src/lib.rs").display());

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target_dir = out.join("proof-world-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .current_dir(&proof)
        .env("CARGO_TARGET_DIR", &target_dir)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .status();

    let wasm = target_dir.join("wasm32-unknown-unknown/release/proof_world.wasm");
    if matches!(status, Ok(status) if status.success()) && wasm.is_file() {
        println!("cargo:rustc-cfg=proof_world_wasm");
        println!("cargo:rustc-env=PROOF_WORLD_WASM={}", wasm.display());
    } else {
        println!(
            "cargo:warning=proof-world was not built for wasm32; the browser runner test will skip"
        );
    }
}
