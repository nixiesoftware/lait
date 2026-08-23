//! The display plane over lait's own overlay.
//!
//! The coordinator's HTTP surface served where the fabric already reaches:
//! a session ALPN beside `FREIGHT`/`LIVE`/`EXEC`, addressed by endpoint id,
//! no listening port and no inbound hole. The TCP listener in
//! [`super::http`] remains for the one case that needs it — televisions on
//! the same LAN, which cannot speak the overlay — and this is every other
//! case: a router bridging receivers in from anywhere, a placement reached
//! across the world, a machine behind a NAT nobody will configure.
//!
//! Each accepted bidirectional flow carries ordinary HTTP/1.1 — the same
//! router, byte for byte, that the TCP path serves. There is no TLS layer
//! here because there is no hop to protect: the overlay connection is
//! end-to-end encrypted to this daemon's endpoint identity already, which is
//! stronger than the self-signed certificate the TCP path pins.

use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Result;
use comms::{RecvFlow, SendFlow, Transport};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
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

/// Serve the display router over every connection arriving on the transport.
///
/// The transport is expected to register only [`DISPLAY_ALPN`]; anything else
/// is refused by closing, never half-served. Runs until `stop` says so or the
/// transport shuts.
pub async fn serve_display_overlay(
    transport: std::sync::Arc<dyn Transport>,
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
            incoming = transport.accept_connection() => {
                let Some(incoming) = incoming else { break };
                if incoming.alpn != DISPLAY_ALPN {
                    incoming.connection.close(0, b"unknown display plane");
                    continue;
                }
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

/// One overlay flow as the byte stream hyper serves on.
///
/// The comms flow traits are message-shaped (`write_all`, `read_chunk`), so
/// each poll drives an owned in-flight future that carries the flow half with
/// it and hands it back on completion — ownership passing rather than a
/// self-borrow, and the stored future is what makes every poll resumable.
struct FlowIo {
    send: Option<Box<dyn SendFlow>>,
    recv: Option<Box<dyn RecvFlow>>,
    /// Bytes read but not yet handed to the caller.
    buffered: Vec<u8>,
    read_in_flight: Option<ReadInFlight>,
    write_in_flight: Option<WriteInFlight>,
}

type BoxedPoll<T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;
type ReadInFlight = BoxedPoll<(Box<dyn RecvFlow>, Result<Option<Vec<u8>>>)>;
/// Carries how many bytes the completed write accepted, because
/// `poll_write` must answer with exactly that on the poll that resolves.
type WriteInFlight = BoxedPoll<(Box<dyn SendFlow>, Result<()>, usize)>;

impl FlowIo {
    fn new(send: Box<dyn SendFlow>, recv: Box<dyn RecvFlow>) -> Self {
        Self {
            send: Some(send),
            recv: Some(recv),
            buffered: Vec::new(),
            read_in_flight: None,
            write_in_flight: None,
        }
    }

    fn drain(&mut self, out: &mut ReadBuf<'_>) {
        let take = self.buffered.len().min(out.remaining());
        out.put_slice(&self.buffered[..take]);
        self.buffered.drain(..take);
    }
}

fn broken(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, error.to_string())
}

impl AsyncRead for FlowIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.buffered.is_empty() {
            self.drain(out);
            return Poll::Ready(Ok(()));
        }
        let mut in_flight = match self.read_in_flight.take() {
            Some(in_flight) => in_flight,
            None => {
                let Some(mut recv) = self.recv.take() else {
                    // A finished flow reads as a clean end, repeatably.
                    return Poll::Ready(Ok(()));
                };
                Box::pin(async move {
                    let read = recv.read_chunk(READ_CHUNK).await;
                    (recv, read)
                })
            }
        };
        match in_flight.as_mut().poll(cx) {
            Poll::Pending => {
                self.read_in_flight = Some(in_flight);
                Poll::Pending
            }
            Poll::Ready((recv, Ok(Some(bytes)))) => {
                self.recv = Some(recv);
                self.buffered = bytes;
                self.drain(out);
                Poll::Ready(Ok(()))
            }
            // Clean end: the recv half is dropped, and later polls answer the
            // same way through the `None` arm above.
            Poll::Ready((_, Ok(None))) => Poll::Ready(Ok(())),
            Poll::Ready((_, Err(error))) => Poll::Ready(Err(broken(error))),
        }
    }
}

impl AsyncWrite for FlowIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut in_flight = match self.write_in_flight.take() {
            Some(in_flight) => in_flight,
            None => {
                let Some(mut send) = self.send.take() else {
                    return Poll::Ready(Err(broken("display overlay flow is closed")));
                };
                let owned = bytes.to_vec();
                let accepted = owned.len();
                Box::pin(async move {
                    let wrote = send.write_all(&owned).await;
                    (send, wrote, accepted)
                })
            }
        };
        match in_flight.as_mut().poll(cx) {
            Poll::Pending => {
                self.write_in_flight = Some(in_flight);
                Poll::Pending
            }
            Poll::Ready((send, Ok(()), accepted)) => {
                self.send = Some(send);
                Poll::Ready(Ok(accepted))
            }
            Poll::Ready((_, Err(error), _)) => Poll::Ready(Err(broken(error))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // `write_all` completes only once the transport accepted the bytes, so
        // the only thing to flush is an in-flight write.
        if let Some(mut in_flight) = self.write_in_flight.take() {
            return match in_flight.as_mut().poll(cx) {
                Poll::Pending => {
                    self.write_in_flight = Some(in_flight);
                    Poll::Pending
                }
                Poll::Ready((send, Ok(()), _)) => {
                    self.send = Some(send);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready((_, Err(error), _)) => Poll::Ready(Err(broken(error))),
            };
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.as_mut().poll_flush(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        if let Some(mut send) = self.send.take() {
            if let Err(error) = send.finish() {
                return Poll::Ready(Err(broken(error)));
            }
        }
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use comms::policy::Network;
    use comms::{DefaultTransport, Protocols};
    use std::sync::Arc;
    use std::time::Duration;

    /// The plane end to end, with no relay and no listener: an Isolated
    /// overlay pair, a dial by direct address, one bidirectional flow, and an
    /// ordinary HTTP/1.1 exchange over it against a real axum router. This is
    /// what the daemon serves and what the router will bridge into; the
    /// coordinator's own routes ride it unchanged because they are the same
    /// `axum::Router` type this test hands in.
    #[tokio::test]
    async fn the_display_router_answers_over_the_overlay() {
        let serving = Arc::new(
            DefaultTransport::new(
                &[81u8; 32],
                &Network::Isolated,
                Protocols {
                    framed: &[],
                    session: &[DISPLAY_ALPN],
                },
            )
            .await
            .expect("bind serving overlay endpoint"),
        );
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
        let server = tokio::spawn(serve_display_overlay(serving.clone(), app, stop_rx));

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
    }
}
