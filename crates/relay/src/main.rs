//! `lait-relay` — stand up the relay half of `LAIT_NETWORK=local`.
//!
//! A lait space that wants to work without depending on n0's public mesh needs a
//! relay somebody owns. This is that relay, as one command on one box.
//!
//! # The three deployments, shortest first
//!
//! **Behind a proxy or tunnel**, which is most of them. Something in front already
//! holds the certificate — Caddy, nginx, a Cloudflare tunnel — so the relay serves
//! plain HTTP and is told what name the world sees:
//!
//! ```sh
//! lait-relay --http 127.0.0.1:8080 --advertise https://relay.example.com
//! ```
//!
//! **On a LAN**, with no public name and nothing in front:
//!
//! ```sh
//! lait-relay --http 0.0.0.0:8080 --advertise 10.0.0.5:8080
//! ```
//!
//! **Public, holding its own certificate.** Point a DNS name at the box, leave
//! port 80 reachable for the ACME challenge, and run:
//!
//! ```sh
//! lait-relay --domain relay.example.com --contact ops@example.com \
//!            --cache /var/lib/lait-relay --staging
//! ```
//!
//! Drop `--staging` once a staging run has succeeded. Keep `--cache` in both:
//! without it every restart re-orders a certificate, and Let's Encrypt's rate
//! limits turn a crash loop into a day-long outage.
//!
//! Whichever shape, the relay prints the one line every peer needs:
//!
//! ```text
//! LAIT_NETWORK=local LAIT_RELAY=https://relay.example.com
//! ```
//!
//! # What this does not do yet
//!
//! It serves **everyone who can reach it**. `iroh-relay` has an access-control
//! seam that could restrict a relay to the devices of one space's members, and a
//! private relay almost certainly wants that. It is not wired here, so treat a
//! public deployment as public: relay traffic stays end-to-end encrypted and
//! unreadable to the relay, but who-talks-to-whom and how much is visible to
//! whoever runs it, and bandwidth is open to anyone who finds it.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use comms::relay::{RelayCertificate, RelayHome};

const USAGE: &str = "\
lait-relay — the server half of LAIT_NETWORK=local

  --http <addr>         where the HTTP service binds        [0.0.0.0:80]
  --advertise <host>    the host peers should dial; needed whenever the bind
                        address is not one a peer can reach (0.0.0.0, or
                        anything behind a proxy). A bare host gets the scheme
                        the certificate implies; an explicit http:// or
                        https:// is left alone.

  Automatic certificates (Let's Encrypt) — omit all of these to serve plain HTTP:
  --domain <name>       a name to certify; repeat for more. The first is what
                        peers are told to use.
  --contact <email>     ACME account contact; repeat for more.
  --https <addr>        where the HTTPS service binds       [0.0.0.0:443]
  --cache <dir>         where to persist the account key and certificate.
                        Strongly recommended: without it a restart re-orders.
  --staging             use Let's Encrypt staging. Untrusted certificates,
                        generous rate limits. Do the first run this way.

  --quic <addr>         bind QUIC address discovery, which is what lets peers
                        learn their public address and holepunch. Requires a
                        certificate.
  --metrics <addr>      serve Prometheus metrics here.
  --help                this text.";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let home = parse_args()?;
    let relay = comms::relay::host(home).await?;

    println!("relay is up.");
    if let Some(addr) = relay.http_addr() {
        println!("  http    {addr}");
    }
    if let Some(addr) = relay.https_addr() {
        println!("  https   {addr}");
    }
    if let Some(addr) = relay.quic_addr() {
        println!("  quic    {addr}");
    }
    println!("\nGive every peer in the space this, and nothing else:\n");
    println!("  LAIT_NETWORK=local LAIT_RELAY={}\n", relay.relay_url());
    println!("Ctrl-C to stop.");

    tokio::signal::ctrl_c().await.context("wait for ctrl-c")?;
    println!("\nstopping…");
    relay.shutdown().await?;
    println!("stopped.");
    Ok(())
}

fn parse_args() -> Result<RelayHome> {
    let mut http: Option<SocketAddr> = None;
    let mut https: Option<SocketAddr> = None;
    let mut quic: Option<SocketAddr> = None;
    let mut metrics: Option<SocketAddr> = None;
    let mut advertise: Option<String> = None;
    let mut domains: Vec<String> = Vec::new();
    let mut contact: Vec<String> = Vec::new();
    let mut cache: Option<PathBuf> = None;
    let mut staging = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow!("{arg} wants a value"));
        match arg.as_str() {
            "--http" => http = Some(value()?.parse().context("--http")?),
            "--https" => https = Some(value()?.parse().context("--https")?),
            "--quic" => quic = Some(value()?.parse().context("--quic")?),
            "--metrics" => metrics = Some(value()?.parse().context("--metrics")?),
            "--advertise" => advertise = Some(value()?),
            "--domain" => domains.push(value()?),
            // ACME wants `mailto:` URIs and operators think in email addresses.
            // Accepting both and normalising is friendlier than an error that
            // teaches the operator an ACME detail they did not need to learn.
            "--contact" => {
                let raw = value()?;
                contact.push(if raw.contains(':') {
                    raw
                } else {
                    format!("mailto:{raw}")
                });
            }
            "--cache" => cache = Some(PathBuf::from(value()?)),
            "--staging" => staging = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown argument {other}\n\n{USAGE}")),
        }
    }

    // Naming a domain is what asks for a certificate. Nothing else does, so an
    // operator who names none gets plain HTTP without having to say so.
    let certificate = if domains.is_empty() {
        if https.is_some() || cache.is_some() || staging {
            return Err(anyhow!(
                "--https, --cache and --staging only mean something with --domain"
            ));
        }
        RelayCertificate::None
    } else {
        if contact.is_empty() {
            return Err(anyhow!(
                "--domain needs a --contact: Let's Encrypt will not issue without one"
            ));
        }
        if cache.is_none() {
            eprintln!(
                "warning: no --cache, so every restart re-orders a certificate. Let's \
                 Encrypt rate-limits that, and a restart loop becomes an outage."
            );
        }
        RelayCertificate::Automatic {
            https: https.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 443))),
            domains,
            contact,
            cache,
            staging,
        }
    };

    Ok(RelayHome {
        http: http.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 80))),
        certificate,
        quic,
        metrics,
        advertise,
    })
}
