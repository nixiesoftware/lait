//! The join-from-link claim, whole: a browser worker holding nothing but a
//! fresh device seed and an invite link joins a real Space over
//! `lait/contact/2` — real ticket, real ledger and replica on real OPFS, real
//! transport through a local relay, the production Contact grammar frame for
//! frame — from a native daemon holding real issue data. No daemon ever
//! touches this seed: the worker self-incepts, pushes its pending admission
//! request on its own dial's reverse phase, the founder redeems it, and the
//! membership + sealed keys arrive on a later pull of the await loop.
//!
//! The harness (`ci/browser-live-space.sh`) founds the Space, writes issues,
//! mints a single-use invite, and bakes the rendezvous in at compile time:
//! the invite ticket, the fresh seed, the relay, the approach peer.
//!
//! The enter itself lives in `wasm_probe::space_pull`, shared with the
//! engine-over-pulled-Space claim so the two cannot drift.

#![cfg(all(target_arch = "wasm32", feature = "probe-contact"))]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::space_pull::{pull_space, unhex32, PulledSpace};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// The admitted actor this device's ledger replay resolves.
fn my_actor(pulled: &PulledSpace) -> String {
    let mut inner = match pulled.authority.0.lock() {
        Ok(inner) => inner,
        Err(poisoned) => poisoned.into_inner(),
    };
    let me = inner.me.clone();
    inner
        .ledger
        .actor_plane()
        .actor_of_device(&me)
        .expect("the ledger admits this device")
        .as_str()
        .to_string()
}

#[wasm_bindgen_test]
async fn a_browser_tab_enters_a_real_space_from_the_invite_alone() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed = unhex32(option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX"));
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");
    let expect_bodies: u64 = option_env!("LIVE_EXPECT_BODIES")
        .expect("harness sets LIVE_EXPECT_BODIES")
        .parse()
        .expect("a count");

    let mut pulled = pull_space(relay, seed, ticket, |_| {}).await;
    assert!(pulled.outcome.bytes_moved > 0, "material moved");

    // The enter resolved: the founder redeemed the pushed request, the pulled
    // ledger admits this device, the keyring can unseal, the replica holds
    // the Space's bodies, and the manifest root is published.
    let actor = my_actor(&pulled);
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

    // The reload claim: entering AGAIN from the same seed and the same
    // single-use invite — fresh local ledger, fresh replica, as a reloaded
    // tab or a new browser profile would — re-mints the byte-identical
    // deterministic inception, so it is the SAME actor, the founder's
    // redemption is idempotent, and the invite nonce is not burned.
    let second = pull_space(relay, seed, ticket, |_| {}).await;
    assert_eq!(
        my_actor(&second),
        actor,
        "a re-enter resolves the same actor — the deterministic inception held"
    );
}
