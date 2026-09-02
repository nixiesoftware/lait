//! The plan-invalidator, measured: does the REAL Issues runner — typst, the
//! CRDT fabric, the whole product, 39 MiB of it — even instantiate under a
//! browser tab's memory, run by the browser's own WebAssembly? This is S7.2's
//! first, load-bearing question, and the answer here is yes.
//!
//! It proves the module loads — it compiles, fits, and (taking entropy from a
//! `lait.random` host import and linking no iroh) resolves its imports with no
//! wasm-bindgen runtime — AND that it builds its whole service and answers
//! Describe with its reviewed identity. Answering a real Call over the pulled
//! ledger is the next step (it needs the ledger composed in the Worker); this
//! is the runner up and identifying itself.
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

#[wasm_bindgen_test]
fn the_real_issues_runner_answers_describe() {
    use world_runner::wasm_abi::GuestInit;
    use world_runner::{no_detached_callbacks, HostedRunner, Operation, Reply};
    let mut instance = WebInstance::launch(
        ISSUES_RUNNER,
        GuestInit {
            world: "com.lait.issues".into(),
            version: "0.9.5".into(),
            release: "local".into(),
        },
    )
    .expect("the Issues runner builds its service and answers init");
    assert_eq!(instance.descriptor().world, "com.lait.issues");
    let mut cb = |_: &str, _: &[u8]| Err("unexpected callback".to_string());
    let reply = instance
        .open()
        .unwrap()
        .dispatch(Operation::Describe, &mut cb, no_detached_callbacks())
        .expect("Describe answers");
    assert!(matches!(reply, Reply::Descriptor(d) if d.implementation != [0;32]));
}
