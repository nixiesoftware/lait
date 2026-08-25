//! The reach router: a blind splice from Web-PKI receivers to identity
//! coordinators over the display overlay.
//!
//! One TCP listener, expected behind the deployment's TLS terminator (the
//! same Caddy-fronted shape `lait-post` serves under). Per connection it
//! reads exactly one HTTP request head — enough to learn the `Host` — takes
//! the first label as the identity, resolves it to a display-plane endpoint,
//! dials that endpoint over the overlay, replays the head, and then **splices
//! bytes both ways without interpreting them**. Streaming, keep-alive, and
//! WebSocket upgrades all ride through, because after the head this is a
//! pipe, not a proxy.
//!
//! What the router deliberately is not: an authority. It holds no coordinator
//! key, mints nothing, and terminates nothing the receiver verifies —
//! everything above TLS is HMAC-authenticated end to end, and a router that
//! substituted a coordinator would produce pairing words that match nothing
//! anybody approves.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use comms::policy::Network;
use comms::{DefaultTransport, FlowIo, PeerId, Protocols, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The display plane's session ALPN — the same constant the coordinator
/// serves; spelled here because the router is deliberately outside the main
/// binary and a shared constants crate for one value would be ceremony.
const DISPLAY_ALPN: &[u8] = b"lait/display/1";

/// Bound on one request head. A head that has not ended by now is not a
/// request this splice carries.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// How a label becomes an overlay endpoint.
///
/// The production resolver asks the registry; tests hand in a map. Sync,
/// because the one network implementation uses the same blocking client the
/// rest of the tree does — callers hop through `spawn_blocking`.
trait Resolver: Send + Sync {
    fn endpoint(&self, label: &str) -> Option<PeerId>;
}

/// Resolution through the registry's public HTTP surface.
struct RegistryResolver {
    base: String,
}

impl Resolver for RegistryResolver {
    fn endpoint(&self, label: &str) -> Option<PeerId> {
        #[derive(serde::Deserialize)]
        struct Resolved {
            endpoint: String,
        }
        let response = ureq::get(&format!("{}/registry/{label}", self.base))
            .timeout(Duration::from_secs(10))
            .call()
            .ok()?;
        let resolved: Resolved = response.into_json().ok()?;
        mechanics::ids::DeviceId::parse(&resolved.endpoint)
    }
}

/// A fixed label table, for tests and for a deployment small enough to be a
/// file.
struct StaticResolver(std::collections::BTreeMap<String, PeerId>);

impl Resolver for StaticResolver {
    fn endpoint(&self, label: &str) -> Option<PeerId> {
        self.0.get(label).cloned()
    }
}

/// The first label of the request's `Host`, lowercased — the identity this
/// connection is for. `None` when the head carries no usable host, which is a
/// request this splice has nowhere to send.
fn label_of(head: &str) -> Option<String> {
    let host = head
        .split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("host")
                .then(|| value.trim().to_ascii_lowercase())
        })?;
    let bare = host.split(':').next().unwrap_or(&host);
    let label = bare.split('.').next()?;
    (!label.is_empty()).then(|| label.to_string())
}

/// Read one request head off the socket, returning the head bytes (through
/// the terminating blank line) and any body bytes that arrived with them.
async fn read_head(tcp: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buffered = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let read = tcp.read(&mut chunk).await.context("read request head")?;
        if read == 0 {
            bail!("connection ended before a request head");
        }
        buffered.extend_from_slice(chunk.get(..read).unwrap_or_default());
        if buffered.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(buffered);
        }
        if buffered.len() > MAX_HEAD_BYTES {
            bail!("request head exceeded {MAX_HEAD_BYTES} bytes");
        }
    }
}

async fn splice(
    mut tcp: TcpStream,
    transport: Arc<DefaultTransport>,
    resolver: Arc<dyn Resolver>,
) -> Result<()> {
    let buffered = read_head(&mut tcp).await?;
    let head_end = buffered
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
        .unwrap_or(buffered.len());
    let head = String::from_utf8_lossy(buffered.get(..head_end).unwrap_or_default()).to_string();
    let Some(label) = label_of(&head) else {
        tcp.write_all(
            b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await
        .ok();
        bail!("request named no resolvable host");
    };
    let resolved = {
        let resolver = resolver.clone();
        let label = label.clone();
        tokio::task::spawn_blocking(move || resolver.endpoint(&label))
            .await
            .context("resolve label")?
    };
    let Some(peer) = resolved else {
        // Coarse on purpose: an unbound label and an unroutable one answer
        // identically, so the router is not an existence oracle beyond what
        // the registry already publishes.
        tcp.write_all(
            b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await
        .ok();
        bail!("label '{label}' did not resolve");
    };
    let connection = transport
        .connect_session(peer, DISPLAY_ALPN)
        .await
        .with_context(|| format!("dial the coordinator for '{label}'"))?;
    let (mut send, recv) = connection.open_bi().await.context("open the splice flow")?;
    // Replay everything already read — the head and whatever body arrived
    // with it — then splice. From here the router understands nothing.
    send.write_all(&buffered).await.context("replay the head")?;
    let mut flow = FlowIo::new(send, recv);
    match tokio::io::copy_bidirectional(&mut tcp, &mut flow).await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(anyhow!(error)).context("splice splice"),
    }
}

async fn serve(
    listener: TcpListener,
    transport: Arc<DefaultTransport>,
    resolver: Arc<dyn Resolver>,
) -> Result<()> {
    loop {
        let (tcp, peer) = listener.accept().await.context("accept receiver")?;
        let transport = transport.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            if let Err(error) = splice(tcp, transport, resolver).await {
                tracing::debug!(%error, %peer, "splice ended");
            }
        });
    }
}

fn resolver_from_env() -> Result<Arc<dyn Resolver>> {
    if let Ok(base) = std::env::var("REACH_REGISTRY") {
        return Ok(Arc::new(RegistryResolver {
            base: base.trim_end_matches('/').to_string(),
        }));
    }
    // REACH_ROUTES: `label=deviceid,label=deviceid` — the file-sized deployment.
    if let Ok(routes) = std::env::var("REACH_ROUTES") {
        let mut table = std::collections::BTreeMap::new();
        for entry in routes.split(',').filter(|entry| !entry.trim().is_empty()) {
            let (label, device) = entry
                .split_once('=')
                .ok_or_else(|| anyhow!("REACH_ROUTES entry '{entry}' is not label=device"))?;
            let peer = mechanics::ids::DeviceId::parse(device.trim())
                .ok_or_else(|| anyhow!("REACH_ROUTES '{label}' names an invalid device id"))?;
            table.insert(label.trim().to_ascii_lowercase(), peer);
        }
        if table.is_empty() {
            bail!("REACH_ROUTES named no routes");
        }
        return Ok(Arc::new(StaticResolver(table)));
    }
    bail!("set REACH_REGISTRY (registry base URL) or REACH_ROUTES (label=deviceid,…)")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let listen: SocketAddr = std::env::var("REACH_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8091".to_string())
        .parse()
        .context("REACH_LISTEN is not a socket address")?;
    let resolver = resolver_from_env()?;
    // The router is a peer like any other: its own seed, the deployment's
    // network policy, no session ALPNs of its own to serve.
    let seed_hex = std::env::var("REACH_SEED").context(
        "set REACH_SEED to this router's 64-hex identity seed — a router is a peer, not a nobody",
    )?;
    let seed_bytes = (0..32)
        .map(|at| {
            let pair = seed_hex.get(at * 2..at * 2 + 2)?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect::<Option<Vec<u8>>>()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| anyhow!("REACH_SEED is not 64 hex characters"))?;
    let network = Network::from_env().context("LAIT_NETWORK / LAIT_RELAY")?;
    let transport = Arc::new(
        DefaultTransport::new(
            &seed_bytes,
            &network,
            Protocols {
                framed: &[],
                session: &[],
            },
        )
        .await
        .context("bind the router's overlay endpoint")?,
    );
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    tracing::info!(%listen, "reach router serving");
    tokio::select! {
        served = serve(listener, transport, resolver) => served,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_is_the_first_host_label_and_ports_do_not_confuse_it() {
        let head = "GET /head/v1/instance HTTP/1.1\r\nHost: Acme.foundation.pub:443\r\n\r\n";
        assert_eq!(label_of(head), Some("acme".to_string()));
        let bare = "GET / HTTP/1.1\r\nhost: acme\r\n\r\n";
        assert_eq!(label_of(bare), Some("acme".to_string()));
        let none = "GET / HTTP/1.1\r\naccept: */*\r\n\r\n";
        assert_eq!(label_of(none), None);
    }

    /// The splice end to end: a real TCP client, the router, and a
    /// coordinator-shaped overlay server, all in this process over Isolated
    /// transports. The response crossing back proves the splice carries both
    /// directions; nothing here interprets a byte after the head.
    #[tokio::test]
    async fn a_receiver_reaches_its_coordinator_through_the_splice() {
        use hyper::server::conn::http1;
        use hyper_util::rt::TokioIo;
        use hyper_util::service::TowerToHyperService;

        // The coordinator: serves HTTP over the display ALPN.
        let coordinator = Arc::new(
            DefaultTransport::new(
                &[91u8; 32],
                &Network::Isolated,
                Protocols {
                    framed: &[],
                    session: &[DISPLAY_ALPN],
                },
            )
            .await
            .expect("bind coordinator endpoint"),
        );
        let app = axum::Router::new().route(
            "/head/v1/instance",
            axum::routing::get(|| async { "spliced-coordinator" }),
        );
        let serving = coordinator.clone();
        tokio::spawn(async move {
            while let Some(incoming) = serving.accept_connection().await {
                let app = app.clone();
                tokio::spawn(async move {
                    while let Ok(Some((send, recv))) = incoming.connection.accept_bi().await {
                        let service = app.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(FlowIo::new(send, recv));
                            let _ = http1::Builder::new()
                                .half_close(true)
                                .serve_connection(io, TowerToHyperService::new(service))
                                .await;
                        });
                    }
                });
            }
        });

        // The router: its own endpoint, taught the coordinator's address the
        // way the Isolated ticket path teaches one.
        let router_transport = Arc::new(
            DefaultTransport::new(
                &[92u8; 32],
                &Network::Isolated,
                Protocols {
                    framed: &[],
                    session: &[],
                },
            )
            .await
            .expect("bind router endpoint"),
        );
        let routes = coordinator
            .advertised_routes(Duration::from_secs(3))
            .await
            .expect("coordinator advertises direct routes");
        router_transport.learn(coordinator.my_id(), &routes);
        let resolver: Arc<dyn Resolver> = Arc::new(StaticResolver(
            [("acme".to_string(), coordinator.my_id())].into(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind splice");
        let splice_addr = listener.local_addr().expect("splice address");
        tokio::spawn(serve(listener, router_transport, resolver));

        // The receiver: plain HTTP at the splice, host names the identity.
        let mut tcp = TcpStream::connect(splice_addr)
            .await
            .expect("reach the splice");
        tcp.write_all(
            b"GET /head/v1/instance HTTP/1.1\r\nhost: acme.foundation.pub\r\nconnection: close\r\n\r\n",
        )
        .await
        .expect("send request");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), tcp.read_to_end(&mut response))
            .await
            .expect("spliced response timed out")
            .expect("read response");
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200"), "spliced: {text}");
        assert!(
            text.contains("spliced-coordinator"),
            "the body crossed: {text}"
        );
    }

    /// An unresolvable label answers coarsely and cheaply, and never dials.
    #[tokio::test]
    async fn an_unbound_label_is_refused_without_a_dial() {
        let transport = Arc::new(
            DefaultTransport::new(
                &[93u8; 32],
                &Network::Isolated,
                Protocols {
                    framed: &[],
                    session: &[],
                },
            )
            .await
            .expect("bind"),
        );
        let resolver: Arc<dyn Resolver> = Arc::new(StaticResolver(Default::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(serve(listener, transport, resolver));

        let mut tcp = TcpStream::connect(addr).await.expect("connect");
        tcp.write_all(b"GET / HTTP/1.1\r\nhost: nobody.foundation.pub\r\n\r\n")
            .await
            .expect("send");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), tcp.read_to_end(&mut response))
            .await
            .expect("refusal timed out")
            .expect("read");
        assert!(
            String::from_utf8_lossy(&response).starts_with("HTTP/1.1 502"),
            "coarse refusal: {}",
            String::from_utf8_lossy(&response)
        );
    }
}
