//! The native half of the browser DATAGRAM liveness check: accept a multi-flow
//! session connection and echo every unreliable datagram back.
//!
//! The Live plane carries carets/presence exclusively as unreliable datagrams
//! (`connection.send_datagram`/`read_datagram`), never on a stream. Before a
//! browser tab can be a Live-plane peer, we must know a datagram round-trips
//! through the real transport from a wasm worker at all — the browser's only
//! path is the relay's WebSocket, and datagram support there is unproven. This
//! example is the native accepter for that spike; `wasm-probe/tests/
//! live_datagram.rs` is the browser dialer, and `ci/browser-live.sh --datagram`
//! wires them together in headless Chrome.
//!
//! `LAIT_NETWORK` / `LAIT_RELAY` are read exactly as the daemon reads them.

use anyhow::{Context as _, Result};
use comms::policy::Network;
use comms::{DefaultFactory, Protocols, TransportFactory};

/// The datagram-probe protocol: one connection, datagrams echoed back.
const DATAGRAM_ALPN: &[u8] = b"lait/probe/datagram/1";

/// A fixed identity so the harness never has to parse more than one line.
const SEED: [u8; 32] = [11u8; 32];

#[tokio::main]
async fn main() -> Result<()> {
    let network = Network::from_env().context("LAIT_NETWORK / LAIT_RELAY")?;
    // Register the ALPN as a SESSION protocol — multi-flow connections, which
    // is what carries datagrams (a plain framed stream does not).
    let protocols = Protocols {
        framed: &[],
        session: &[DATAGRAM_ALPN],
    };
    let transport = DefaultFactory
        .build(&SEED, &network, protocols)
        .await
        .context("build transport")?;
    println!("device id {}", transport.my_id().as_str());
    println!(
        "echoing datagrams on {}…",
        String::from_utf8_lossy(DATAGRAM_ALPN)
    );

    while let Some(incoming) = transport.accept_connection().await {
        if incoming.alpn != DATAGRAM_ALPN {
            continue;
        }
        println!("datagram probe from {}", incoming.from.short());
        let connection = incoming.connection;
        // Report the negotiated datagram capacity — `None` is the honest "this
        // path carries no datagrams" answer the spike is looking for.
        match connection.datagram_capacity() {
            Some(bytes) => println!("datagram capacity {bytes} bytes"),
            None => println!("datagram capacity NONE — path negotiated no datagrams"),
        }
        let outcome: Result<()> = async {
            while let Some(payload) = connection.read_datagram().await? {
                connection
                    .send_datagram(&payload)
                    .context("echo datagram")?;
            }
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => println!("datagram stream ended cleanly"),
            Err(error) => println!("datagram probe ended: {error:#}"),
        }
    }
    Ok(())
}
