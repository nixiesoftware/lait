//! The live-network claim: a real `IrohTransport` inside a real browser
//! worker, rendezvousing through a real (local) lait relay and completing an
//! identity-bound framed round trip with a native peer. Everything between
//! the browser and the native process is production transport — relay
//! rendezvous over WebSocket, QUIC through it, ALPN dispatch, framing;
//! nothing is mocked and no public infrastructure is touched.
//!
//! Run by `ci/browser-live.sh`, which starts `lait-relay` and
//! `comms/examples/live_echo`, then compiles this test with the rendezvous
//! baked in: wasm tests have no environment at runtime, so the harness passes
//! `LIVE_RELAY_URL` and `LIVE_PEER_ID` at build time via `option_env!`.

#![cfg(all(
    target_arch = "wasm32",
    feature = "probe-comms",
    feature = "probe-mechanics"
))]

use comms::policy::{LocalNet, Network};
use comms::{DefaultFactory, Protocols, Transport, TransportFactory};
use mechanics::ids::DeviceId;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const LIVE_ALPN: &[u8] = b"lait/probe/live/1";

fn rendezvous() -> (String, DeviceId) {
    let relay = option_env!("LIVE_RELAY_URL")
        .expect("ci/browser-live.sh sets LIVE_RELAY_URL at compile time");
    let peer = option_env!("LIVE_PEER_ID")
        .expect("ci/browser-live.sh sets LIVE_PEER_ID at compile time");
    let peer = DeviceId::parse(peer).expect("LIVE_PEER_ID parses as a device id");
    (relay.to_owned(), peer)
}

#[wasm_bindgen_test]
async fn a_browser_peer_reaches_a_native_peer_through_the_relay() {
    let (relay, peer) = rendezvous();
    let network = Network::Local(LocalNet {
        relays: vec![relay],
    });
    let transport = DefaultFactory
        .build(&[9u8; 32], &network, Protocols::framed(&[]))
        .await
        .expect("the browser endpoint comes up against the local relay");

    // Under Local there is no discovery: learning {id, relay} is the whole
    // resolution story, exactly as the daemon does it.
    transport.learn(peer.clone(), &[]);
    let mut stream = transport
        .connect(peer, LIVE_ALPN)
        .await
        .expect("dial the native peer through the relay");

    let payload = b"the tab says hello through its own relay".to_vec();
    stream.send(&payload).await.expect("frame sent");
    let echoed = stream
        .recv()
        .await
        .expect("echo frame arrives")
        .expect("stream stays open for the echo");
    assert_eq!(echoed, payload, "the native peer echoed our bytes");

    // A second frame proves the stream survived the first exchange.
    let again = b"and a second frame rides the same stream".to_vec();
    stream.send(&again).await.expect("second frame sent");
    let echoed = stream
        .recv()
        .await
        .expect("second echo arrives")
        .expect("stream still open");
    assert_eq!(echoed, again);
    // Dropping the dialer's stream is the close signal; the accepter's
    // wait_closed resolves from it.
}
