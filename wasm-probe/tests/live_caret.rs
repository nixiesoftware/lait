//! The Live plane in a tab: a browser worker joins a real daemon's Live plane
//! and publishes presence — the p2p live-caret path, no cloud.
//!
//! Sub-slice A proves the whole client-side path against alice's REAL daemon
//! acceptor: dial `lait/session/1`, complete the Open/Accept handshake (which
//! only succeeds if the daemon ADMITS the tab as a member — the in-tab
//! admission from the enter stage), open the CONTROL flow and subscribe, and
//! send a presence datagram. A successful Accept is itself the membership
//! proof, and it settles the last transport unknown: uni/bi FLOWS work over a
//! browser session connection (the datagram spike proved only datagrams).
//!
//! Carets (an anchored payload on a Field scope) and the receive half + viewer
//! wiring are the next sub-slices; presence needs no anchor, so it is the
//! smallest honest first proof.
//!
//! Runs under `ci/browser-live-space.sh` (needs the relay + a founded Space +
//! the invite the tab enters with).

#![cfg(all(target_arch = "wasm32", feature = "probe-dispatch"))]

use mechanics::actor::device_from_seed;
use mechanics::station::Key;
use runtime::transient::{Target, TransientPayload};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::live_client::LiveClient;
use wasm_probe::space_pull::{pull_space, unhex32};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

#[wasm_bindgen_test]
async fn a_tab_joins_the_live_plane_and_publishes_presence() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed = unhex32(option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX"));
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");

    // Enter (idempotent — same deterministic actor as the other stages) to
    // stand up the transport, learn alice's approach station, and be an
    // admitted member so the Live acceptor resolves this device to an actor.
    let pulled = pull_space(relay, seed, ticket, |_| {}).await;
    let local = Key::from_device(&device_from_seed(&seed)).expect("the tab's station key");

    // Dial alice's Live plane — the SAME peer the Contact pull dialed — and
    // complete the handshake. A refusal here would mean alice does not admit
    // the tab as a Live peer; an Accept proves membership and that the uni
    // flows the handshake rides work in a browser.
    let live = LiveClient::connect(pulled.transport.as_ref(), &pulled.space, &local, &pulled.responder)
        .await
        .expect("the tab joins alice's Live plane over lait/session/1");

    // Say what the tab is looking at (a Body scope needs no anchor), then
    // publish presence as a datagram bound to this session's epoch.
    let scope = Target::Body {
        world: "com.lait.issues".to_string(),
        body: [7u8; 16],
    };
    live.subscribe(vec![scope.clone()])
        .await
        .expect("the CONTROL subscribe crosses (open_bi works in a browser)");
    assert!(
        live.publish(scope, TransientPayload::Presence),
        "the presence datagram fit the path and was sent"
    );
}
