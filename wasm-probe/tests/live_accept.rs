//! The browser-ACCEPT spike: can a wasm iroh endpoint ACCEPT an incoming
//! relay-routed connection — not just dial one? `live.rs` proved the dial; this
//! proves the accept, the one fact that decides whether a browser tab can serve
//! peers itself (Contact admission, the Live plane) or whether that role must
//! live in a cloud companion.
//!
//! The browser builds a transport that accepts `lait/probe/live/1`, and a native
//! `live_dialer` (started beside it by `ci/browser-accept-spike.sh`) dials it
//! through a local relay and sends a frame; the browser echoes it. A completed
//! echo means a wasm endpoint accepted an inbound connection over the relay.
//! The browser's identity is the fixed seed the dialer derives its peer id from.

#![cfg(all(
    target_arch = "wasm32",
    feature = "probe-comms",
    feature = "probe-mechanics"
))]

use comms::policy::{LocalNet, Network};
use comms::{DefaultFactory, Protocols, Transport, TransportFactory};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const LIVE_ALPN: &[u8] = b"lait/probe/live/1";
/// The fixed identity `live_dialer` derives the target peer id from.
const SEED: [u8; 32] = [11u8; 32];

fn relay() -> String {
    option_env!("LIVE_RELAY_URL")
        .expect("ci/browser-accept-spike.sh sets LIVE_RELAY_URL at compile time")
        .to_owned()
}

#[wasm_bindgen_test]
async fn a_browser_peer_accepts_a_native_dial_through_the_relay() {
    let network = Network::Local(LocalNet {
        relays: vec![relay()],
    });
    // Register the probe ALPN on the ACCEPT side — the whole question is whether
    // an inbound dial on it is ever delivered to `accept()` in wasm.
    let transport = DefaultFactory
        .build(&SEED, &network, Protocols::framed(&[LIVE_ALPN]))
        .await
        .expect("the browser endpoint comes up against the local relay");

    // Block until a peer dials us. If wasm cannot accept relay-routed inbound
    // connections at all, this never resolves and the test times out — the
    // negative answer the spike is looking for.
    let mut incoming = loop {
        let inc = transport
            .accept()
            .await
            .expect("the accept side stays open until a dial arrives");
        if inc.alpn == LIVE_ALPN {
            break inc;
        }
    };

    // Echo frames back until the dialer's send side ends. That end can arrive
    // as a clean Ok(None) (its finish) or as an Err (its connection drop
    // surfaced as a reset over the relay) — both mean "the dialer is done", and
    // the PROOF is that we echoed at least one frame it dialed us with. The
    // capability the spike measures is settled the moment an inbound frame
    // reaches this accepter's stream at all.
    let mut echoed = 0u32;
    loop {
        match incoming.stream.recv().await {
            Ok(Some(frame)) => {
                incoming
                    .stream
                    .send(&frame)
                    .await
                    .expect("echo the frame back");
                echoed += 1;
            }
            // The dialer finished or dropped — either way the exchange is over.
            Ok(None) | Err(_) => break,
        }
    }
    assert!(echoed >= 1, "an inbound relay-routed frame reached accept()");

    // Close our send side so the dialer's own wait_closed resolves (it does,
    // and its process exiting 0 is the harness's proof). We do NOT wait_closed
    // here: over the relay a browser accepter is not reliably delivered the
    // peer's connection-close, so blocking on it hangs the tab — a teardown
    // asymmetry, not a transport gap. finish() is the clean-close signal the
    // dialer needs; our proof is already recorded above.
    incoming.stream.finish().await.expect("finish the echo");
}
