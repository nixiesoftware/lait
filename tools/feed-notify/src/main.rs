//! `lait-feed-notify` — run the notify relay. See the library doc for what it
//! is and is not.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use lait_feed_notify::{parse_pubkeys, router_priming, serve_priming, Board, PRIME_EVERY};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lait_feed_notify=info".into()),
        )
        .init();

    let mut http: Option<SocketAddr> = None;
    let mut state: Option<PathBuf> = None;
    let mut pubkeys: Vec<String> = Vec::new();
    let mut feed: Option<String> = None;
    let mut prime_every = PRIME_EVERY;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => {
                http = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--http needs an address"))?
                        .parse()
                        .context("--http is not an address")?,
                );
            }
            "--state" => {
                state = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("--state needs a path"))?,
                ));
            }
            "--pubkey" => {
                pubkeys.push(
                    args.next()
                        .ok_or_else(|| anyhow!("--pubkey needs a hex key"))?,
                );
            }
            "--feed" => {
                feed = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--feed needs a base URL"))?,
                );
            }
            "--prime-every" => {
                prime_every = Duration::from_secs(
                    args.next()
                        .ok_or_else(|| anyhow!("--prime-every needs seconds"))?
                        .parse()
                        .context("--prime-every is not a number of seconds")?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "lait-feed-notify --http <addr> --pubkey <hex> [--pubkey <hex>...] \
                     [--feed <base-url> [--prime-every <secs>]] [--state <file>]"
                );
                return Ok(());
            }
            other => return Err(anyhow!("unrecognized argument `{other}`")),
        }
    }
    let http = http.ok_or_else(|| anyhow!("--http is required"))?;
    let pubkeys = parse_pubkeys(&pubkeys)?;
    let mut board = Board::open(pubkeys, state.clone())?;
    if let Some(feed) = &feed {
        board = board.with_feed(feed);
    }
    let board = Arc::new(Mutex::new(board));
    if feed.is_some() {
        tokio::spawn(serve_priming(board.clone(), prime_every));
    }

    let listener = tokio::net::TcpListener::bind(http)
        .await
        .with_context(|| format!("bind {http}"))?;
    tracing::info!(%http, state = ?state, feed = ?feed, "the notify relay is open");
    axum::serve(listener, router_priming(board, prime_every))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serve")
}
