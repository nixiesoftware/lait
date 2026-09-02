//! Slice 8.4: the shippable packaging boundary, end to end. One `boot` call
//! stands the whole browser engine up — compile the runner once, instantiate
//! the three nested layers, pull the Space over the relay onto OPFS, compose
//! the daemon's own Station, dock the member, wire the world-agnostic dispatch
//! — and hands back the `#[wasm_bindgen]` handle the viewer's Worker holds for
//! the tab's life. Then the handle answers frames as JSON strings (the exact
//! `postMessage` vocabulary the Worker relays) and installs a live re-pull.
//!
//! This is the packaging risk settled by execution, not assertion: a handle
//! holding non-`Send` `Rc`/`RemoteClient` state returns to JS from an async
//! export, survives, and answers a later call back in. `tests/dispatch.rs`
//! proved the composition; this proves it packages.
//!
//! Runs under `ci/browser-live-space.sh` (needs the relay + a founded Space).

#![cfg(all(target_arch = "wasm32", feature = "probe-dispatch", issues_runner_wasm))]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::handle::boot;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const ISSUES_RUNNER: &[u8] = include_bytes!(env!("ISSUES_RUNNER_WASM"));

const WORLD_FRAME: &str =
    r#"{"lait":"rpc","id":1,"verb":"world","request":{"cmd":"project_list","page":{}}}"#;

#[wasm_bindgen_test]
async fn boot_packages_the_engine_and_a_frame_crosses() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed_hex = option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX");
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");

    // The whole stand-up behind one call — and the identity strings are inputs,
    // never literals baked into the packaging, which is what keeps `boot`
    // world-agnostic: a release and a local copy reach it the same way.
    let handle = boot(
        relay.to_string(),
        seed_hex.to_string(),
        ticket.to_string(),
        ISSUES_RUNNER.to_vec(),
        "com.lait.issues".to_string(),
        "0.9.5".to_string(),
        "local".to_string(),
        "issues".to_string(),
    )
    .await
    .expect("the engine boots in a tab from one packaging call");

    // A world frame crosses the whole seam and answers, as a JSON string — the
    // Worker relays exactly this. alice's ENG project comes back.
    let world = handle
        .handle_link(WORLD_FRAME)
        .expect("the world frame answers");
    assert!(
        world.contains("\"lait\":\"reply\"") && world.contains("\"id\":1"),
        "a reply frame naming its id: {world}"
    );
    assert!(
        world.contains("ENG"),
        "the world frame carried alice's ENG across the packaging boundary: {world}"
    );

    // The spaces verb: one served row, honestly shaped, through the same string
    // boundary.
    let spaces = handle
        .handle_link(r#"{"lait":"rpc","id":2,"verb":"spaces"}"#)
        .expect("the spaces frame answers");
    assert!(
        spaces.contains("\"kind\":\"served\""),
        "a served row with no daemon probe fields: {spaces}"
    );

    // The control plane, world-agnostic: a daemon-only act refuses `not_hosted`,
    // never the head's wrong-mount refusal.
    let host = handle
        .handle_link(r#"{"lait":"rpc","id":3,"verb":"host","request":{"cmd":"member_remove"}}"#)
        .expect("the host frame answers");
    assert!(
        host.contains("\"error_kind\":\"not_hosted\""),
        "a daemon-only act refuses not_hosted through the boundary: {host}"
    );

    // A live re-pull installs through the Station's own writer from the SAME
    // handle — the reactivity seam, packaged. Idempotent here (bob's write is
    // already local), so it proves the install path executes and leaves the
    // live core intact.
    let _moved = handle
        .repull()
        .await
        .expect("a live re-pull installs through the packaged handle");

    // The handle still answers after the re-pull — the state it holds across JS
    // calls survived the convergence.
    let again = handle
        .handle_link(WORLD_FRAME)
        .expect("the world frame answers after the re-pull");
    assert!(
        again.contains("ENG"),
        "the packaged live core still answers after a re-pull: {again}"
    );
}
