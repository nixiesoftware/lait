//! `lait-net` — a self-sovereign L3 tunnel between lait devices, Linux-first.
//!
//! Each node's address is derived from its own key (see [`lait_net::ula_from_key`]).
//! Run one on each device, cross-configure the peers, and IP packets between
//! their derived addresses ride the tunnel — `ping6` the peer's address to see
//! it work.
//!
//! ```text
//! # On device A (its seed is 32 bytes of hex):
//! sudo lait-net --seed <A-seed-hex> --listen 0.0.0.0:51820 \
//!               --peer <B-host>:51820=<B-pubkey-hex>
//! # `lait-net --seed <A-seed-hex> --print` shows A's pubkey and address.
//! ```

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;

use lait_net::{ipv6_destination, parse_key_hex, to_hex, ula_from_key};

const DEFAULT_DEV: &str = "lait0";
const DEFAULT_LISTEN: &str = "0.0.0.0:51820";
// A TUN read buffer larger than any plausible interface MTU.
const MTU_CEILING: usize = 65_535;

struct Args {
    seed: [u8; 32],
    dev: String,
    listen: SocketAddr,
    peers: Vec<(Ipv6Addr, SocketAddr)>,
    print_only: bool,
}

fn usage() -> ! {
    eprintln!(
        "lait-net — a self-sovereign L3 tunnel between lait devices (Linux)\n\
         \n\
         USAGE:\n\
         \x20 lait-net --seed <64-hex> [--dev <name>] [--listen <ip:port>]\n\
         \x20          [--peer <host:port>=<pubkey-64-hex>]... [--print]\n\
         \n\
         Each node's tunnel address is fd00::/8 derived from its key. Give each\n\
         peer its UDP endpoint and its public key; the address follows. Needs\n\
         CAP_NET_ADMIN (run with sudo). Not encrypted yet — use a trusted link."
    );
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut seed: Option<[u8; 32]> = None;
    let mut dev = DEFAULT_DEV.to_string();
    let mut listen = DEFAULT_LISTEN.to_string();
    let mut peers = Vec::new();
    let mut print_only = false;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--seed" => {
                seed = Some(
                    parse_key_hex(&argv.next().unwrap_or_default())
                        .unwrap_or_else(|| fail("--seed must be 64 hex characters")),
                );
            }
            "--dev" => dev = argv.next().unwrap_or_else(|| fail("--dev needs a value")),
            "--listen" => {
                listen = argv
                    .next()
                    .unwrap_or_else(|| fail("--listen needs a value"))
            }
            "--peer" => {
                let spec = argv.next().unwrap_or_else(|| fail("--peer needs a value"));
                let (endpoint, key_hex) = spec
                    .split_once('=')
                    .unwrap_or_else(|| fail("--peer must be <host:port>=<pubkey-hex>"));
                let udp: SocketAddr = endpoint
                    .parse()
                    .unwrap_or_else(|_| fail("--peer endpoint must be host:port"));
                let key = parse_key_hex(key_hex)
                    .unwrap_or_else(|| fail("--peer key must be 64 hex characters"));
                peers.push((ula_from_key(&key), udp));
            }
            "--print" => print_only = true,
            "-h" | "--help" => usage(),
            other => fail(&format!("unexpected argument: {other}")),
        }
    }

    let seed = seed.unwrap_or_else(|| fail("--seed is required"));
    let listen = listen
        .parse()
        .unwrap_or_else(|_| fail("--listen must be ip:port"));
    Args {
        seed,
        dev,
        listen,
        peers,
        print_only,
    }
}

fn fail(message: &str) -> ! {
    eprintln!("lait-net: {message}");
    std::process::exit(2);
}

fn main() -> std::io::Result<()> {
    let args = parse_args();
    let device = mechanics::actor::device_from_seed(&args.seed);
    let pubkey = device
        .key_bytes()
        .expect("a seeded device carries an endpoint key");
    let address = ula_from_key(&pubkey);

    println!("device  {}", device.as_str());
    println!("pubkey  {}", to_hex(&pubkey));
    println!("address {address}");
    for (peer_addr, peer_udp) in &args.peers {
        println!("peer    {peer_addr} via {peer_udp}");
    }
    if args.print_only {
        return Ok(());
    }

    let (mut tun_reader, actual) = lait_net::tun::open(&args.dev)?;
    let peer_addrs: Vec<Ipv6Addr> = args.peers.iter().map(|(addr, _)| *addr).collect();
    lait_net::tun::configure(&actual, address, &peer_addrs)?;
    println!("interface {actual} up");

    let routes: HashMap<Ipv6Addr, SocketAddr> = args.peers.iter().copied().collect();
    let socket = Arc::new(UdpSocket::bind(args.listen)?);
    let mut tun_writer = tun_reader.try_clone()?;

    // Inbound: a datagram from a peer is a raw IP packet; write it to the TUN.
    let inbound = {
        let socket = Arc::clone(&socket);
        std::thread::spawn(move || -> std::io::Result<()> {
            let mut buf = vec![0u8; MTU_CEILING];
            loop {
                let (n, _from) = socket.recv_from(&mut buf)?;
                // A short write to a TUN would corrupt a packet; treat it as fatal.
                tun_writer.write_all(&buf[..n])?;
            }
        })
    };

    // Outbound: a packet leaving the TUN is sent to the peer that owns its
    // destination address. A packet for an unknown destination is dropped.
    let outbound = std::thread::spawn(move || -> std::io::Result<()> {
        let mut buf = vec![0u8; MTU_CEILING];
        loop {
            let n = tun_reader.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            if let Some(dest) = ipv6_destination(&buf[..n]) {
                if let Some(peer) = routes.get(&dest) {
                    socket.send_to(&buf[..n], peer)?;
                }
            }
        }
    });

    match inbound.join() {
        Ok(result) => result?,
        Err(_) => return Err(std::io::Error::other("inbound thread panicked")),
    }
    match outbound.join() {
        Ok(result) => result?,
        Err(_) => return Err(std::io::Error::other("outbound thread panicked")),
    }
    Ok(())
}
