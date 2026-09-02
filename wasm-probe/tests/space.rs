//! The 4b claim, whole: a browser worker joins a real Space as a
//! pre-admitted member device and pulls it over `lait/contact/2` — real
//! ticket, real ledger and replica on real OPFS, real transport through a
//! local relay, the production Contact grammar frame for frame — from a
//! native daemon holding real issue data.
//!
//! The harness (`ci/browser-live-space.sh`) founds the Space, writes issues,
//! admits a scratch second daemon whose seed IT chose, stops that daemon
//! (one DeviceId, one holder), and bakes the rendezvous in at compile time:
//! the invite ticket, the admitted seed, the relay, the approach peer.
//!
//! The pull itself lives in `wasm_probe::space_pull`, shared with the
//! engine-over-pulled-Space claim so the two cannot drift.

#![cfg(all(target_arch = "wasm32", feature = "probe-contact"))]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::space_pull::{pull_space, unhex32};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

#[wasm_bindgen_test]
async fn a_browser_member_pulls_a_real_space_over_contact() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed = unhex32(option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX"));
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");
    let expect_bodies: u64 = option_env!("LIVE_EXPECT_BODIES")
        .expect("harness sets LIVE_EXPECT_BODIES")
        .parse()
        .expect("a count");

    let mut pulled = pull_space(relay, seed, ticket, |_| {}).await;
    assert!(pulled.outcome.bytes_moved > 0, "material moved");

    // The pulled ledger admitted us: the keyring can unseal, the replica
    // holds the Space's bodies, and the manifest root is published.
    assert!(
        pulled.replica.body_count() >= expect_bodies,
        "pulled {} bodies, expected at least {expect_bodies}",
        pulled.replica.body_count()
    );
    let root = pulled.replica.published_root().expect("a published root");

    // Convergence's own idempotence check: pulling again moves the grammar,
    // changes nothing, and the root stands.
    let _ = pulled.pull_again().await;
    assert_eq!(
        pulled.replica.published_root().expect("still published"),
        root,
        "a repeated pull is idempotent"
    );
}
