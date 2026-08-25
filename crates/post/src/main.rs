//! `lait-post` — run the Post.
//!
//! Three endpoints and a timer. The whole service is a signature check in front
//! of a blob store, which is what makes it affordable to operate at all — the
//! design it replaced was a mail server, and everything that bought beyond this
//! was DNS, deliverability reputation, spam handling and a hostile-MIME parsing
//! surface.
//!
//! ```sh
//! lait-post --http 127.0.0.1:8090 --root /var/lib/lait-post
//! lait-post --http 127.0.0.1:8090 --root /var/lib/lait-post //!           --directory-project the-foundation-498604
//! ```
//!
//! With `--directory-project`, the identity directory is mounted under
//! `/directory` in this same process, over Firestore in that project. Without
//! it the Post runs exactly as it did, which is what keeps the directory from
//! becoming a thing the carrier depends on.
//!
//! It holds no keys, mints no credentials, and terminates no TLS: put it behind
//! something that does. A carrier that cannot read what it carries does not need
//! to be trusted with much, but it should still not be reachable in the clear.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use lait_post::http::{now, router, Shared};
use lait_post::{FsStore, Post};

/// How often the sweep runs. Frequent enough that an expired deposit is gone in
/// minutes rather than days, cheap enough to be uninteresting.
const SWEEP_EVERY: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lait_post=info".into()),
        )
        .init();

    let mut http: Option<SocketAddr> = None;
    let mut root: Option<String> = None;
    let mut directory_project: Option<String> = None;
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
            "--root" => root = Some(args.next().ok_or_else(|| anyhow!("--root needs a path"))?),
            "--directory-project" => {
                directory_project = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--directory-project needs a GCP project id"))?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "lait-post --http <addr> --root <dir> [--directory-project <gcp-project>]"
                );
                return Ok(());
            }
            other => return Err(anyhow!("unrecognized argument `{other}`")),
        }
    }
    let http = http.ok_or_else(|| anyhow!("--http is required"))?;
    let root = root.ok_or_else(|| anyhow!("--root is required"))?;

    let store = FsStore::open(&root).context("open the deposit root")?;
    let shared: Shared = Arc::new(Mutex::new(Post::new(store)));

    let sweeper = Arc::clone(&shared);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SWEEP_EVERY);
        loop {
            ticker.tick().await;
            let collected = match sweeper.lock() {
                Ok(mut post) => post.sweep(now()),
                Err(poisoned) => poisoned.into_inner().sweep(now()),
            };
            if collected > 0 {
                tracing::info!(collected, "swept deposits nobody came for");
            }
        }
    });

    let mut app = router(shared);

    // Mounted only when asked for. A default would point every operator at a
    // project they do not own, and — worse for this service in particular — a
    // directory that answers "not available" because it was misconfigured is
    // indistinguishable at the client from a person who does not exist.
    if let Some(project) = directory_project {
        let store =
            lait_directory::FirestoreStore::open(&project, lait_directory::Credentials::Metadata);
        let directory: lait_directory::http::Shared<lait_directory::FirestoreStore> =
            Arc::new(Mutex::new(lait_directory::Service::new(store)));
        app = app.merge(lait_directory::http::router(directory));
        tracing::info!(project, "the directory is mounted under /directory");

        // The label registry rides the same opt-in: it is the public half of
        // the same deployment, and an operator who mounted no directory has
        // no registry to speak of either. Same project, its own collections —
        // `registry-bindings` holds the curated allocation, `registry-routes`
        // what identities publish, `registry-chronicle` the committed log the
        // registrar signs its heads over.
        let registry_store =
            lait_directory::FirestoreStore::open(&project, lait_directory::Credentials::Metadata);
        let (seed, ephemeral) = lait_directory::registry::chronicle_seed_from_env()
            .context("registry chronicle seed")?;
        if ephemeral {
            tracing::warn!(
                "REGISTRY_CHRONICLE_SEED is unset — the chronicle signs under an identity \
                 that will not survive a restart"
            );
        }
        let registrar = lait_directory::registry::Registrar::open(registry_store, seed)
            .context("open registrar")?;
        let registry = Arc::new(Mutex::new(registrar));
        app = app.merge(lait_directory::registry::router(registry));
        tracing::info!(project, "the registry is mounted under /registry");
    }

    let listener = tokio::net::TcpListener::bind(http)
        .await
        .with_context(|| format!("bind {http}"))?;
    tracing::info!(%http, root, "the Post is open");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serve")?;
    Ok(())
}
