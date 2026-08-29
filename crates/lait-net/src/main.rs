//! `lait-net` — bring up lait's L3 tunnel on a device. A thin front over
//! [`netstack`]: it parses arguments and hands them to the boundary, naming no
//! transport type of its own.
//!
//! Each node's address is derived from its own key, and its transport identity
//! *is* that same key — one seed gives a device its tunnel address and its
//! `PeerId`. Name peers by their public key; `ping6` a peer's derived address
//! to see it work.
//!
//! ```text
//! # On a LAN, no relay, no discovery (Isolated), reach a peer directly:
//! sudo lait-net --seed <A-seed-hex> --network isolated \
//!               --peer <B-pubkey-hex>@<B-host>:<B-port>
//! # `lait-net --seed <A-seed-hex> --print` shows A's pubkey and address.
//! ```

use std::net::{Ipv6Addr, SocketAddr};

use anyhow::{Context, Result};
use mechanics::ids::DeviceId;

use netstack::carry::{Config, Peer};
use netstack::{parse_key_hex, ula_from_key, LocalNet, Network};

const DEFAULT_DEV: &str = "lait0";

struct Cli {
    seed: [u8; 32],
    dev: String,
    network: Network,
    peers: Vec<Peer>,
    print_only: bool,
}

fn usage() -> ! {
    eprintln!(
        "lait-net — bring up lait's L3 tunnel on a device (Linux)\n\
         \n\
         USAGE:\n\
         \x20 lait-net --seed <64-hex> [--dev <name>] [--network public|local|isolated]\n\
         \x20          [--relay <url>]... [--peer <pubkey-64-hex>[@<addr>[,<addr>]]]... [--print]\n\
         \n\
         Each node's tunnel address and PeerId both come from its key. Name each\n\
         peer by public key; its fd00::/8 address follows. Under `isolated` give\n\
         each peer a direct address; `public` resolves by discovery; `local`\n\
         rendezvous through the `--relay` you supply. Needs CAP_NET_ADMIN (sudo).\n\
         The carry is encrypted (QUIC)."
    );
    std::process::exit(2);
}

fn fail(message: &str) -> ! {
    eprintln!("lait-net: {message}");
    std::process::exit(2);
}

fn parse_peer(spec: &str) -> Result<Peer> {
    let (key_hex, addrs) = match spec.split_once('@') {
        Some((key, rest)) => (key, Some(rest)),
        None => (spec, None),
    };
    let key = parse_key_hex(key_hex).context("peer key must be 64 hex characters")?;
    let direct = match addrs {
        Some(list) => list
            .split(',')
            .map(|a| {
                a.parse::<SocketAddr>()
                    .context("peer address must be host:port")
            })
            .collect::<Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    Ok(Peer {
        id: DeviceId::from_key_bytes(&key),
        ula: ula_from_key(&key),
        direct,
    })
}

fn parse_cli() -> Cli {
    let mut seed: Option<[u8; 32]> = None;
    let mut dev = DEFAULT_DEV.to_string();
    let mut network: Option<String> = None;
    let mut relays: Vec<String> = Vec::new();
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
            "--network" => {
                network = Some(
                    argv.next()
                        .unwrap_or_else(|| fail("--network needs a value")),
                );
            }
            "--relay" => relays.push(argv.next().unwrap_or_else(|| fail("--relay needs a value"))),
            "--peer" => {
                let spec = argv.next().unwrap_or_else(|| fail("--peer needs a value"));
                peers.push(parse_peer(&spec).unwrap_or_else(|error| fail(&format!("{error:#}"))));
            }
            "--print" => print_only = true,
            "-h" | "--help" => usage(),
            other => fail(&format!("unexpected argument: {other}")),
        }
    }

    let seed = seed.unwrap_or_else(|| fail("--seed is required"));
    let network = match network.as_deref() {
        None | Some("isolated") => Network::Isolated,
        Some("public") => Network::Public,
        Some("local") => {
            if relays.is_empty() {
                fail("--network local needs at least one --relay <url>");
            }
            Network::Local(LocalNet { relays })
        }
        Some(other) => fail(&format!(
            "unknown --network '{other}' (public|local|isolated)"
        )),
    };

    Cli {
        seed,
        dev,
        network,
        peers,
        print_only,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = parse_cli();
    let device = mechanics::actor::device_from_seed(&cli.seed);
    let address = device
        .key_bytes()
        .map(|key| ula_from_key(&key))
        .expect("a seeded device carries an endpoint key");

    // The device id string is the hex of its public key — its pubkey and its
    // name are one value.
    println!("pubkey  {}", device.as_str());
    println!("address {address}");
    for peer in &cli.peers {
        let via = if peer.direct.is_empty() {
            "discovery".to_string()
        } else {
            peer.direct
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        println!("peer    {} via {via}", peer.ula);
    }
    if cli.print_only {
        return Ok(());
    }

    let (tun_reader, actual) = netstack::tun::open(&cli.dev).context("open TUN")?;
    let peer_addrs: Vec<Ipv6Addr> = cli.peers.iter().map(|peer| peer.ula).collect();
    netstack::tun::configure(&actual, address, &peer_addrs).context("configure TUN")?;
    let tun_writer = tun_reader.try_clone().context("clone TUN handle")?;
    println!("interface {actual} up; carrying over lait transport");

    netstack::carry::run(
        Config {
            seed: cli.seed,
            network: cli.network,
            peers: cli.peers,
        },
        tun_reader,
        tun_writer,
    )
    .await
}
