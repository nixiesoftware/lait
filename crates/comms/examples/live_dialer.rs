//! The dialer half of the browser-ACCEPT spike: dial a given peer on the
//! live-probe ALPN, send a frame, and require the echo. Where `live_echo`
//! accepts and the browser dials, here the browser ACCEPTS and this native
//! process dials it — the one fact the spike must establish: whether a wasm
//! iroh endpoint can accept an incoming relay-routed connection at all.
//!
//! `ci/browser-accept-spike.sh` runs this against a local `lait-relay`, given
//! the browser's device id (printed by the wasm accept test) as the one arg.
//! `LAIT_NETWORK` / `LAIT_RELAY` are read as the daemon reads them.

use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use comms::policy::Network;
use comms::{DefaultFactory, Protocols, TransportFactory};

const LIVE_ALPN: &[u8] = b"lait/probe/live/1";
const SEED: [u8; 32] = [23u8; 32];
/// The browser accepter's fixed identity — the same seed `live_accept.rs`
/// builds with, so this dialer derives its peer id with no coordination.
const BROWSER_SEED: [u8; 32] = [11u8; 32];

#[tokio::main]
async fn main() -> Result<()> {
    let peer = mechanics::actor::device_from_seed(&BROWSER_SEED);
    let network = Network::from_env().context("LAIT_NETWORK / LAIT_RELAY")?;
    let transport = DefaultFactory
        .build(&SEED, &network, Protocols::framed(&[]))
        .await
        .context("build the dialer transport")?;
    // Under Local there is no discovery: learning {id, relay} is the whole
    // resolution story, so the relay routes our dial to the browser.
    transport.learn(peer.clone(), &[]);

    // The browser is coming up in a headless Chrome beside us and its FIRST
    // wasm compile can take minutes, so the dial is retried against a
    // wall-clock deadline (not a fixed try count): we must still be knocking
    // when the tab finally launches and starts accepting, or the tab times its
    // own accept out with nothing on the wire. The harness starts us before it
    // compiles the tab; ACCEPT_SPIKE_DEADLINE_SECS covers the cold build.
    let deadline_secs: u64 = std::env::var("ACCEPT_SPIKE_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let deadline = std::time::Instant::now() + Duration::from_secs(deadline_secs);
    let payload = b"a native peer dials the browser through the relay".to_vec();
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match transport.connect(peer.clone(), LIVE_ALPN).await {
            Ok(mut stream) => {
                stream
                    .send(&payload)
                    .await
                    .context("send the probe frame")?;
                let echoed = stream
                    .recv()
                    .await
                    .context("await the echo")?
                    .context("the browser closed before echoing")?;
                if echoed != payload {
                    bail!("the browser echoed different bytes");
                }
                // Close our send side cleanly so the tab's recv() sees a clean
                // end (Ok(None)) rather than a reset — its accept loop then
                // finishes and wait_closed resolves, exactly as live_echo does.
                stream.finish().await.context("finish the dialer stream")?;
                stream.wait_closed().await;
                println!("ACCEPT-SPIKE OK: the browser accepted the dial and echoed the frame");
                return Ok(());
            }
            Err(error) => {
                if std::time::Instant::now() >= deadline {
                    bail!(
                        "could not reach the browser accepter after {attempt} tries \
                         in {deadline_secs}s: {error:#}"
                    );
                }
                tokio::time::sleep(Duration::from_millis(750)).await;
            }
        }
    }
}
