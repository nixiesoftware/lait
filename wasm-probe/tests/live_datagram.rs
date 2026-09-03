//! The datagram spike: does an unreliable datagram round-trip from a real
//! browser worker to a native peer through the relay?
//!
//! The Live plane carries carets/presence exclusively as unreliable datagrams
//! over a multi-flow session connection (`connect_session` → `Connection::
//! send_datagram`/`read_datagram`), never on a stream. The stream path is
//! already proven (`live.rs`); this proves — or refutes — the datagram path,
//! which is the go/no-go for a p2p live-caret client in a tab. The browser's
//! only route to the peer is the relay's WebSocket, and datagram support across
//! it is unproven: `datagram_capacity()` may be `None` (a refusal), in which
//! case carets cannot ride datagrams and the Live wire needs a stream lane.
//!
//! Run by `ci/browser-live.sh --datagram`, which starts `lait-relay` and
//! `comms/examples/live_datagram`, then compiles this with the rendezvous baked
//! in via `option_env!` (wasm tests have no runtime environment).

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

const DATAGRAM_ALPN: &[u8] = b"lait/probe/datagram/1";

fn rendezvous() -> (String, DeviceId) {
    let relay = option_env!("LIVE_RELAY_URL")
        .expect("ci/browser-live.sh sets LIVE_RELAY_URL at compile time");
    let peer =
        option_env!("LIVE_PEER_ID").expect("ci/browser-live.sh sets LIVE_PEER_ID at compile time");
    let peer = DeviceId::parse(peer).expect("LIVE_PEER_ID parses as a device id");
    (relay.to_owned(), peer)
}

#[wasm_bindgen_test]
async fn a_browser_peer_round_trips_a_datagram_through_the_relay() {
    let (relay, peer) = rendezvous();
    let network = Network::Local(LocalNet {
        relays: vec![relay],
    });
    // The dialer registers no ALPNs of its own; it opens a session connection.
    let transport = DefaultFactory
        .build(&[9u8; 32], &network, Protocols::framed(&[]))
        .await
        .expect("the browser endpoint comes up against the local relay");
    transport.learn(peer.clone(), &[]);

    let connection = transport
        .connect_session(peer, DATAGRAM_ALPN)
        .await
        .expect("open a multi-flow session to the native peer through the relay");

    // The pivotal measurement: does this path carry datagrams at all? `None`
    // is a refusal, and the whole slice would then need a stream lane instead.
    let capacity = connection.datagram_capacity();
    assert!(
        capacity.is_some(),
        "the browser↔relay path negotiated NO datagram support (capacity None) — \
         carets cannot ride datagrams on this path"
    );

    // A datagram out, the same bytes back. Datagrams are unreliable, so the
    // native echo may need a moment; retry the read a few times before failing.
    let payload = b"a caret-sized datagram from the tab".to_vec();
    connection
        .send_datagram(&payload)
        .expect("send one datagram to the native peer");

    let echoed = connection
        .read_datagram()
        .await
        .expect("read a datagram back")
        .expect("the native peer echoed a datagram before the connection closed");
    assert_eq!(echoed, payload, "the native peer echoed our datagram bytes");
}
