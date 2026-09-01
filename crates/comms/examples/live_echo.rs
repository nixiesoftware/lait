//! The native half of the browser liveness check: accept a framed stream on
//! the live-probe ALPN and echo every frame back.
//!
//! `ci/browser-live.sh` runs this beside a local `lait-relay` and points a
//! headless-Chrome wasm test at both. The echo is not a toy: the dial rides
//! the real transport — relay rendezvous, QUIC over the relay's WebSocket,
//! ALPN dispatch, framed streams — and iroh's TLS mutually authenticates the
//! two `DeviceId`s, so a completed round trip is an identity-bound proof that
//! a browser peer can reach a native one through lait's own relay.
//!
//! `LAIT_NETWORK` / `LAIT_RELAY` are read exactly as the daemon reads them.

use anyhow::{Context as _, Result};
use comms::policy::Network;
use comms::{DefaultFactory, Protocols, TransportFactory};

/// The liveness-probe protocol: whole frames in, the same frames out.
const LIVE_ALPN: &[u8] = b"lait/probe/live/1";

/// A fixed identity so the harness never has to parse more than one line.
const SEED: [u8; 32] = [7u8; 32];

#[tokio::main]
async fn main() -> Result<()> {
    let network = Network::from_env().context("LAIT_NETWORK / LAIT_RELAY")?;
    let transport = DefaultFactory
        .build(&SEED, &network, Protocols::framed(&[LIVE_ALPN]))
        .await
        .context("build transport")?;
    println!("device id {}", transport.my_id().as_str());
    println!("echoing on {}…", String::from_utf8_lossy(LIVE_ALPN));

    while let Some(mut incoming) = transport.accept().await {
        if incoming.alpn != LIVE_ALPN {
            continue;
        }
        println!("probe from {}", incoming.from.short());
        let outcome: Result<()> = async {
            while let Some(frame) = incoming.stream.recv().await? {
                incoming.stream.send(&frame).await?;
            }
            // The accepter's close discipline: finish only queues the end
            // marker, and dropping before the dialer drains would truncate.
            incoming.stream.finish().await?;
            incoming.stream.wait_closed().await;
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => println!("echoed and closed cleanly"),
            Err(error) => println!("probe ended: {error:#}"),
        }
    }
    Ok(())
}
