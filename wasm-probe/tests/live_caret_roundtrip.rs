//! The live-caret round trip: a tab publishes a real caret into an issue's
//! body, and another node (alice's daemon) reads it — carets crossing the
//! network from a daemon-less tab, no cloud.
//!
//! The tab boots the whole engine, resolves the issue's body through the runner
//! (world-agnostic), mints a `fabric::Anchor` for a position in its
//! `description` field, and publishes it over its Live session in a loop —
//! HOLDING the connection open, because a peer's caret is dropped from the
//! table the instant its session ends (`Leaving::drop` forgets it). The
//! harness runs this in the background and polls alice's `{"cmd":"live"}` for
//! the tab's actor as a caret entry while the loop holds the session.
//!
//! `LIVE_CARET_ISSUE` is an issue the harness created ON ALICE with a real body
//! (a bodyless issue has no collaborative field to anchor into), baked in at
//! compile time like the rest of the rendezvous.

#![cfg(all(
    target_arch = "wasm32",
    feature = "probe-dispatch",
    issues_runner_wasm
))]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::handle::boot;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const ISSUES_RUNNER: &[u8] = include_bytes!(env!("ISSUES_RUNNER_WASM"));

/// The `reff` of the list item whose title matches — how the test finds the
/// canonical doc id of the issue the harness created, without depending on the
/// daemon's reply carrying the alias vs the canonical form.
fn reff_of_titled(list: &serde_json::Value, title: &str) -> Option<String> {
    let items = list
        .get("reply")?
        .get("body")?
        .get("page")?
        .get("items")?
        .as_array()?;
    items.iter().find_map(|item| {
        (item.get("title")?.as_str()? == title)
            .then(|| item.get("reff")?.as_str().map(str::to_string))
            .flatten()
    })
}

#[wasm_bindgen_test]
async fn a_tab_publishes_a_caret_another_node_reads() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed_hex = option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX");
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");

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
    .expect("the engine boots in a tab");

    // Find the caret-target issue's canonical doc id (iss_…) from the list —
    // `transient_body` hashes exactly the id the viewer watches by, and the
    // list's `reff` is that canonical form, NOT the "ENG-4" key alias.
    let list = handle
        .handle_link(r#"{"lait":"rpc","id":1,"verb":"world","request":{"cmd":"list","page":{}}}"#)
        .expect("the list answers");
    let list_json: serde_json::Value = serde_json::from_str(&list).expect("the list decodes");
    let issue = reff_of_titled(&list_json, "caret target")
        .unwrap_or_else(|| panic!("the caret-target issue is not in the tab's pull: {list}"));

    // Drive the caret through the viewer's OWN `session:watch` question shape
    // ({issue, cursor:{field, anchor}}), not a bespoke call — the real frame the
    // editor sends. Publish repeatedly, holding the Live session open the whole
    // window so the harness — polling alice concurrently — can catch it.
    let question = format!(
        r#"{{"space":"s","issue":"{issue}","cursor":{{"field":"description","anchor":20}}}}"#
    );
    let mut published = 0u32;
    for _ in 0..100 {
        let sent = handle
            .watch_caret(&question)
            .await
            .expect("the caret publishes without error");
        if sent {
            published += 1;
        }
        n0_future::time::sleep(n0_future::time::Duration::from_millis(300)).await;
    }
    assert!(
        published > 0,
        "the tab published at least one caret datagram over its Live session"
    );
}
