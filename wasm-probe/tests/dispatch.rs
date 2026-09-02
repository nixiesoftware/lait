//! Slice 8.3: a product World RPC crosses the world-agnostic dispatch seam in
//! a browser. The viewer sends a product request (`{cmd:"project_list"}`); the
//! browser engine parses it with the runner's own `ClientAdapter`, forwards it
//! into the runner with `execute`, and answers the runner's callbacks through
//! the browser `ClientHost` — driving world sub-calls back over the composed
//! Session. That last step re-enters the runner (a nested wasm activation on
//! one stack); this test settles whether the module tolerates it. The RPC
//! comes back carrying alice's ENG project — read through the SAME seam the
//! daemon uses, naming no World.
//!
//! Runs under `ci/browser-live-space.sh` (needs the relay + a founded Space).

#![cfg(all(target_arch = "wasm32", feature = "probe-dispatch", issues_runner_wasm))]

use std::sync::Arc;

use mechanics::actor::device_from_seed;
use mechanics::station::Epoch;
use runtime::world::{AuthorityView, Builder, World};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::dispatch::BrowserEngine;
use wasm_probe::runner::WebInstance;
use wasm_probe::space_pull::{pull_space, unhex32};
use world_runner::wasm_abi::GuestInit;
use world_sdk::{RemoteClient, RemoteWorld};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const ISSUES_RUNNER: &[u8] = include_bytes!(env!("ISSUES_RUNNER_WASM"));

#[wasm_bindgen_test]
async fn a_product_world_rpc_crosses_the_dispatch_seam_in_a_browser() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed = unhex32(option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX"));
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");

    // THREE instances of the one module, one per nested guest layer, because a
    // wasm instance cannot be re-entered mid-callback: the client execute
    // (CLIENT_EXECUTE) calls back into the control Handler (APPLICATION_CALL),
    // which calls session.query (QUERY) — three activations that would nest on
    // one instance and trap. Each layer gets its own instance so no instance
    // is ever entered while it is suspended.
    let launch = |tag: &str| {
        let instance = WebInstance::launch(
            ISSUES_RUNNER,
            GuestInit {
                world: "com.lait.issues".into(),
                version: "0.9.5".into(),
                release: "local".into(),
            },
        )
        .unwrap_or_else(|e| panic!("the {tag} runner instance launches: {e}"));
        Arc::new(
            RemoteWorld::connect_runner(Box::new(instance))
                .unwrap_or_else(|e| panic!("the {tag} runner connects: {e}")),
        )
    };

    // Q: the World the Session queries/submits through.
    let world_runner = launch("world");
    let implementation = world_runner.reviewed_implementation();
    let world_id = world_runner.descriptor().id.clone();
    let registry = Builder::new()
        .register_reviewed(world_runner.clone(), implementation)
        .build()
        .expect("the runner's declared contract registers");

    // K: the control Handler that call_world drives (APPLICATION_CALL).
    let control = launch("control");
    // C: the ClientAdapter that runs parse_web/execute (CLIENT_EXECUTE).
    let client_runner = launch("client");
    let _ = RemoteClient::connect(client_runner.clone())
        .expect("the client runner answers a client DESCRIBE");

    // Pull the Space, schemas declared first.
    let pulled = pull_space(relay, seed, ticket, |replica| {
        runtime::browser::declare_schemas(replica, &registry);
    })
    .await;
    assert!(pulled.outcome.bytes_moved > 0, "material moved");

    // Resolve the caller's actor/device from the pulled ledger before it moves
    // into the composed Station.
    let device = device_from_seed(&seed);
    let authority = runtime::browser::LedgerAuthorityView(pulled.authority.clone());
    let actor = authority
        .resolve(&device)
        .map(|resolution| resolution.actor.as_str().to_string())
        .expect("the pulled ledger resolves the member's actor");

    let station = runtime::browser::Station::compose(
        pulled.space.clone(),
        pulled.replica,
        Arc::new(authority),
        registry,
        Epoch::from_u64(1),
    )
    .expect("the browser Station composes");
    let identity = runtime::browser::Station::identity_from_seed(&seed);
    let session = station
        .dock(&world_id, &identity)
        .expect("the admitted member docks");

    let engine = BrowserEngine::new(
        session,
        control,
        client_runner,
        identity,
        actor,
        device.as_str().to_string(),
    )
    .expect("the browser engine composes over the runner");

    // The world-agnostic Call: a product request in, a product answer out,
    // crossing parse_web → execute → the runner's callbacks → the composed
    // Session (re-entering the runner). alice's ENG project comes back.
    let answer = engine
        .world_rpc(serde_json::json!({ "cmd": "project_list", "page": {} }))
        .expect("the product world RPC crosses the dispatch seam and answers");
    let text = serde_json::to_string(&answer).expect("serialize the answer");
    assert!(
        text.contains("ENG"),
        "alice's ENG project crossed the dispatch seam: {text}",
    );
}
