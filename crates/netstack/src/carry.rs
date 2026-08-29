//! The carry: IP packets between two devices over lait's own transport.
//!
//! This replaces the slice-1 UDP splice with a `comms` transport — iroh under
//! the hood, but named only through `comms`, which is lait's sole iroh seam.
//! That buys three things the UDP prototype lacked: **encryption** (QUIC),
//! **lait's own reachability** (its relays and NAT traversal, chosen by
//! `LAIT_NETWORK`), and **reconnection** (a dropped path re-dials on its own).
//!
//! Packets are framed on a raw bidirectional flow as a `u16` big-endian length
//! followed by the packet. One flow per peer carries both directions, split
//! into an independent reader and writer task so traffic never head-of-line
//! blocks itself. The lower `PeerId` dials; the higher accepts — one connection
//! per pair, deterministically.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Ipv6Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use comms::{
    policy::Network, DefaultFactory, PeerId, Protocols, RecvFlow, SendFlow, Transport,
    TransportFactory,
};
use mechanics::ids::DeviceId;

/// The tunnel protocol. Its own generation, independent of every other plane.
const ALPN: &[u8] = b"lait/net/0";
/// The largest packet the tunnel will frame. A generous ceiling above any MTU.
const MAX_PACKET: usize = 65_535;

/// One configured peer: who it is, where its packets are addressed, and — under
/// `Isolated` — how to reach it directly.
pub struct Peer {
    pub id: DeviceId,
    pub ula: Ipv6Addr,
    pub direct: Vec<std::net::SocketAddr>,
}

pub struct Config {
    pub seed: [u8; 32],
    pub network: Network,
    pub peers: Vec<Peer>,
}

/// Outbound routing: a peer's tunnel address → a channel into its live flow.
/// Rebuilt on every reconnect; a packet for a peer with no live flow is
/// dropped, exactly as a packet for an unreachable host would be.
type Routes = Arc<Mutex<HashMap<Ipv6Addr, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>;

/// Run the carry until a fatal transport error. `tun_reader`/`tun_writer` are
/// two handles on the same interface (a `try_clone`d TUN file).
pub async fn run(
    config: Config,
    tun_reader: std::fs::File,
    tun_writer: std::fs::File,
) -> Result<()> {
    // Our own PeerId — the identity the transport binds and the id the
    // dial/accept ordering is decided by. A seed always yields one.
    let me = mechanics::actor::device_from_seed(&config.seed);
    let transport = DefaultFactory
        .build(
            &config.seed,
            &config.network,
            Protocols {
                framed: &[],
                session: &[ALPN],
            },
        )
        .await
        .context("build transport")?;

    // Under Isolated there is no discovery, so teach the transport each peer's
    // direct addresses before any dial.
    for peer in &config.peers {
        if !peer.direct.is_empty() {
            transport.learn(peer.id.clone(), &peer.direct);
        }
    }

    let routes: Routes = Arc::new(Mutex::new(HashMap::new()));

    // Inbound packets from every peer funnel to one blocking writer thread.
    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || tun_write_loop(tun_writer, inbound_rx));

    // Outbound: the blocking TUN reader routes each packet by destination.
    {
        let routes = Arc::clone(&routes);
        std::thread::spawn(move || tun_read_loop(tun_reader, routes));
    }

    // Dial peers with a larger id than ours; accept the rest.
    let mut tasks = Vec::new();
    for peer in config.peers {
        if me.as_str() < peer.id.as_str() {
            let transport = Arc::clone(&transport);
            let routes = Arc::clone(&routes);
            let inbound_tx = inbound_tx.clone();
            tasks.push(tokio::spawn(async move {
                dial_forever(transport, peer, routes, inbound_tx).await;
            }));
        }
    }

    // One accept loop serves every peer that dials us; it runs until a fatal
    // transport error. The dial tasks reconnect on their own.
    let _ = tasks;
    accept_forever(transport, routes, inbound_tx).await
}

/// Dial one peer, serve the connection, and re-dial with backoff when it drops.
async fn dial_forever(
    transport: Arc<dyn Transport>,
    peer: Peer,
    routes: Routes,
    inbound_tx: std::sync::mpsc::Sender<Vec<u8>>,
) {
    loop {
        match transport.connect_session(peer.id.clone(), ALPN).await {
            Ok(connection) => match connection.open_bi().await {
                Ok((send, recv)) => {
                    serve(peer.ula, send, recv, &routes, &inbound_tx).await;
                }
                Err(error) => eprintln!("lait-net: open flow to {}: {error:#}", peer.id.short()),
            },
            Err(error) => eprintln!("lait-net: dial {}: {error:#}", peer.id.short()),
        }
        routes.lock().expect("routes").remove(&peer.ula);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Accept inbound connections forever, serving each on its own tasks.
async fn accept_forever(
    transport: Arc<dyn Transport>,
    routes: Routes,
    inbound_tx: std::sync::mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    loop {
        let Some(incoming) = transport.accept_connection().await else {
            return Ok(());
        };
        let ula = lait_net_ula(&incoming.from);
        let routes = routes.clone();
        let inbound_tx = inbound_tx.clone();
        tokio::spawn(async move {
            match incoming.connection.accept_bi().await {
                Ok(Some((send, recv))) => {
                    serve(ula, send, recv, &routes, &inbound_tx).await;
                }
                Ok(None) => {}
                Err(error) => eprintln!("lait-net: accept flow: {error:#}"),
            }
            routes.lock().expect("routes").remove(&ula);
        });
    }
}

fn lait_net_ula(peer: &PeerId) -> Ipv6Addr {
    crate::ula_for(peer).unwrap_or(Ipv6Addr::UNSPECIFIED)
}

/// Split one flow pair into a writer (drains this peer's route channel) and a
/// reader (frames inbound packets to the TUN), and run until either ends.
async fn serve(
    ula: Ipv6Addr,
    mut send: Box<dyn SendFlow>,
    mut recv: Box<dyn RecvFlow>,
    routes: &Routes,
    inbound_tx: &std::sync::mpsc::Sender<Vec<u8>>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    routes.lock().expect("routes").insert(ula, tx);

    let writer = async move {
        while let Some(packet) = rx.recv().await {
            let len = u16::try_from(packet.len()).unwrap_or(u16::MAX);
            if send.write_all(&len.to_be_bytes()).await.is_err()
                || send.write_all(&packet).await.is_err()
            {
                break;
            }
        }
    };

    let inbound_tx = inbound_tx.clone();
    let reader = async move {
        loop {
            let header = match recv.read_exact(2).await {
                Ok(bytes) => bytes,
                Err(_) => break,
            };
            let len = usize::from(u16::from_be_bytes([header[0], header[1]]));
            if len == 0 || len > MAX_PACKET {
                break;
            }
            match recv.read_exact(len).await {
                Ok(packet) => {
                    if inbound_tx.send(packet).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    tokio::select! {
        _ = writer => {}
        _ = reader => {}
    }
    routes.lock().expect("routes").remove(&ula);
}

/// Blocking: read packets from the TUN and hand each to the peer that owns its
/// destination address. A packet for an unknown destination is dropped.
fn tun_read_loop(mut tun: std::fs::File, routes: Routes) {
    let mut buf = vec![0u8; MAX_PACKET];
    loop {
        let n = match tun.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) => {
                eprintln!("lait-net: TUN read: {error}");
                break;
            }
        };
        if let Some(dest) = crate::ipv6_destination(&buf[..n]) {
            if let Some(tx) = routes.lock().expect("routes").get(&dest) {
                let _ = tx.send(buf[..n].to_vec());
            }
        }
    }
}

/// Blocking: write inbound packets to the TUN. A short write would corrupt a
/// packet, so it is fatal.
fn tun_write_loop(mut tun: std::fs::File, inbound: std::sync::mpsc::Receiver<Vec<u8>>) {
    while let Ok(packet) = inbound.recv() {
        if let Err(error) = tun.write_all(&packet) {
            eprintln!("lait-net: TUN write: {error}");
            break;
        }
    }
}
