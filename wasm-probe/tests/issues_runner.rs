//! The plan-invalidator, measured: does the REAL Issues runner — typst, the
//! CRDT fabric, the whole product, 39 MiB of it — even instantiate under a
//! browser tab's memory, run by the browser's own WebAssembly? This is S7.2's
//! first, load-bearing question, and the answer here is yes.
//!
//! It proves the module loads: it compiles, it fits, and — because the runner
//! now takes its entropy from a `lait.random` host import and links no iroh —
//! its imports resolve without a wasm-bindgen runtime. Building the service in
//! `init` and answering a real RPC over the pulled ledger is the next step
//! (the guest currently traps inside service construction); it is not what
//! this test claims.
//!
//! Runs under `wasm-pack test --headless --chrome --test issues_runner`.
//! Skips honestly when the harness did not build the runner (claim 7).

#![cfg(all(target_arch = "wasm32", feature = "probe-runner", issues_runner_wasm))]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::runner::WebInstance;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const ISSUES_RUNNER: &[u8] = include_bytes!(env!("ISSUES_RUNNER_WASM"));

#[wasm_bindgen_test]
fn the_real_issues_runner_instantiates_under_browser_webassembly() {
    // If this returns, a 39 MiB typst/CRDT World runner compiled and loaded in
    // the browser and its imports resolved — the premise the whole browser
    // execution rests on. The runner is a near-pure core-wasm module: its only
    // capability imports are the ABI's `host_call` and a `lait.random` for
    // entropy, so it needs no wasm-bindgen runtime to instantiate.
    WebInstance::instantiate_module(ISSUES_RUNNER)
        .expect("the real Issues runner instantiates under browser WebAssembly");
}
