//! The display plane over lait's own overlay.
//!
//! The coordinator's HTTP surface served where the fabric already reaches:
//! a session ALPN beside `FREIGHT`/`LIVE`/`EXEC`, addressed by endpoint id,
//! no listening port and no inbound hole. The TCP listener in
//! [`super::http`] remains for the one case that needs it — televisions on
//! the same LAN, which cannot speak the overlay — and this is every other
//! case: a router splicing receivers in from anywhere, a placement reached
//! across the world, a machine behind a NAT nobody will configure.
//!
//! Each accepted bidirectional flow carries ordinary HTTP/1.1 — the same
//! router, byte for byte, that the TCP path serves. There is no TLS layer
//! here because there is no hop to protect: the overlay connection is
//! end-to-end encrypted to this daemon's endpoint identity already, which is
//! stronger than the self-signed certificate the TCP path pins.

use anyhow::Result;
use comms::{ConnectionQueue, FlowIo};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// The display plane's session ALPN.
///
/// Beside the other planes in spirit, in this module in fact: the plane
/// constants in `runtime` are Space-scoped, and the display surface is the
/// identity's — served by the daemon itself, never by a Station.
pub const DISPLAY_ALPN: &[u8] = b"lait/display/1";

/// Ceiling for one overlay read. A pre-allocation bound, not a target.
const READ_CHUNK: usize = 64 * 1024;

/// Serve the display router over every connection arriving on the queue.
///
/// The queue is the identity endpoint's [`DISPLAY_ALPN`] lane, taken once
/// from the transport hub: everything on it is this plane's by construction,
/// and there is no second endpoint under the identity's key for a dialer to
/// reach instead of this one. Runs until `stop` says so or the lane closes.
pub async fn serve_display_overlay(
    mut queue: ConnectionQueue,
    app: axum::Router,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut connections = JoinSet::new();
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            incoming = queue.recv() => {
                let Some(incoming) = incoming else { break };
                let app = app.clone();
                connections.spawn(async move {
                    let connection = incoming.connection;
                    let mut flows = JoinSet::new();
                    // One HTTP/1.1 exchange stream per bidirectional flow, as
                    // many as the peer opens. `Ok(None)` is the peer done;
                    // an error is the connection gone. Both end the loop.
                    while let Ok(Some((send, recv))) = connection.accept_bi().await {
                        let service = app.clone();
                        flows.spawn(async move {
                            let io = TokioIo::new(FlowIo::new(send, recv));
                            // Half-close is the flow's natural shape: an
                            // overlay client finishes its send half the moment
                            // the request is written, and hyper's default
                            // treats that FIN as a dead connection.
                            if let Err(error) = http1::Builder::new()
                                .half_close(true)
                                .serve_connection(io, TowerToHyperService::new(service))
                                .await
                            {
                                tracing::debug!(%error, "display overlay exchange ended");
                            }
                        });
                    }
                    flows.shutdown().await;
                });
            }
        }
    }
    connections.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::transport_hub::TransportHubFactory;
    use comms::policy::Network;
    use comms::{DefaultFactory, DefaultTransport, Protocols, Transport, TransportFactory};
    use std::sync::Arc;
    use std::time::Duration;

    /// The plane end to end, with no relay and no listener: the identity's
    /// hub endpoint on one side, a dial by direct address on the other, one
    /// bidirectional flow, and an ordinary HTTP/1.1 exchange over it against
    /// a real axum router. This is what the daemon serves — the overlay is a
    /// lane of the identity endpoint, not an endpoint of its own — and the
    /// coordinator's own routes ride it unchanged because they are the same
    /// `axum::Router` type this test hands in.
    #[tokio::test]
    async fn the_display_router_answers_over_the_overlay() {
        let hub = TransportHubFactory::new(
            Arc::new(DefaultFactory),
            tokio::sync::watch::channel(None).1,
        );
        let serving = hub
            .identity_transport(&[81u8; 32], &Network::Isolated)
            .await
            .expect("raise the identity endpoint");
        let queue = serving
            .take_session_queue(DISPLAY_ALPN)
            .expect("the display lane is taken once");
        let dialing = DefaultTransport::new(
            &[82u8; 32],
            &Network::Isolated,
            Protocols {
                framed: &[],
                session: &[DISPLAY_ALPN],
            },
        )
        .await
        .expect("bind dialing overlay endpoint");

        let app = axum::Router::new().route(
            "/head/v1/instance",
            axum::routing::get(|| async { "overlay-coordinator" }),
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let server = tokio::spawn(serve_display_overlay(queue, app, stop_rx));

        // Isolated: the dialer learns the server's direct addresses, exactly
        // as a ticket would carry them.
        let routes = serving
            .advertised_routes(Duration::from_secs(3))
            .await
            .expect("serving endpoint advertises direct routes");
        assert!(!routes.is_empty(), "an Isolated endpoint has direct routes");
        dialing.learn(serving.my_id(), &routes);

        let connection = tokio::time::timeout(
            Duration::from_secs(20),
            dialing.connect_session(serving.my_id(), DISPLAY_ALPN),
        )
        .await
        .expect("overlay dial timed out")
        .expect("overlay dial failed");

        let (mut send, mut recv) = connection.open_bi().await.expect("open exchange flow");
        send.write_all(
            b"GET /head/v1/instance HTTP/1.1\r\nhost: overlay\r\nconnection: close\r\n\r\n",
        )
        .await
        .expect("write request");
        send.finish().expect("finish request");

        let mut response = Vec::new();
        while let Some(chunk) = recv.read_chunk(READ_CHUNK).await.expect("read response") {
            response.extend_from_slice(&chunk);
            if response.len() > 64 * 1024 {
                panic!("response exceeded any sane bound");
            }
        }
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.starts_with("HTTP/1.1 200"),
            "the router answered over the overlay: {text}"
        );
        assert!(
            text.contains("overlay-coordinator"),
            "the body is the route's own: {text}"
        );

        stop_tx.send(true).ok();
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
        hub.shutdown().await;
    }
}
