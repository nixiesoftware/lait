//! The optional HTTP adapter, run as a process.
//!
//! This binary is *not* how anything embeds the supervisor. Astrolabe links the
//! library and calls it directly; this exists so the same DTOs and the same
//! safety rules can be driven over a socket for diagnostics and for tests that
//! want a real one. It builds only with the `http` feature.
//!
//! It owns nothing the library does not: the supervisor's construction and
//! shutdown are [`Supervisor::start`] and [`Supervisor::shutdown`], and what is
//! left here is argument resolution, a listener, and a router.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use lait::serve::auth::mint_token;
use lait_workbench::{Config, Supervisor};
use serde::Serialize;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Ready {
    url: String,
    token: String,
    port: u16,
    state_root: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let current_dir = std::env::current_dir().context("resolve current directory")?;
    let state_root = std::env::var_os("LAIT_WORKBENCH_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| current_dir.join("target").join("lait-workbench"));
    let executable = match std::env::var_os("LAIT_BIN") {
        Some(path) => PathBuf::from(path),
        None => sibling_lait_executable()?,
    };
    let port = std::env::var("LAIT_WORKBENCH_PORT")
        .ok()
        .map(|value| value.parse::<u16>())
        .transpose()
        .context("LAIT_WORKBENCH_PORT must be a TCP port")?
        .unwrap_or(0);

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .context("bind workbench loopback server")?;
    let bound = listener.local_addr().context("read workbench address")?;
    let token = mint_token()?;
    // The stream this adapter does not itself consume: every SSE connection
    // opens its own. Held so the channel has a receiver for the supervisor's
    // whole life, which keeps `start`'s ordering guarantee true even when no
    // browser is attached.
    let (supervisor, _signals) =
        Supervisor::start(Config::new(state_root.clone(), executable)).await?;
    let app = lait_workbench::api::router(supervisor.clone(), token.clone(), bound.port());
    let ready = Ready {
        url: format!("http://{bound}"),
        token,
        port: bound.port(),
        state_root: state_root.to_string_lossy().into_owned(),
    };
    println!("{}", serde_json::to_string(&ready)?);

    // Ctrl-C only ends the *serving*; the shutdown itself is the same call on
    // every exit path, so it happens once, below, however we got here.
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    supervisor.shutdown().await;
    result.context("serve workbench API")
}

fn sibling_lait_executable() -> Result<PathBuf> {
    let current = std::env::current_exe().context("locate workbench executable")?;
    let name = if cfg!(windows) { "lait.exe" } else { "lait" };
    Ok(current.with_file_name(name))
}
