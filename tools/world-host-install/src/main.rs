//! Install a first-party World's signed release from the production feed into a
//! daemon identity, so a **headless** daemon can found and host that World's
//! Space.
//!
//! This is the operator seam the Astrolabe client's Install action has and a
//! headless host did not. It changes transport, never authority: it runs the
//! same [`lait::update::world::check`] the client runs, resolving the signed
//! channel pointer over the production feed and verifying it against the keys
//! this binary pins (`FEED_PUBKEYS_HEX`). There is deliberately no key, base
//! URL, or channel override — an operator chooses *which* World to install, not
//! what to trust.
//!
//! It installs into exactly the directory the daemon reads for the same
//! `--home`: `installations_root(Selection::for_identity(home).identity_dir())`.
//! Passing this tool the same `--identity` the daemon is launched with `--home`
//! is what closes the install-vs-host directory seam — the class of bug the
//! client-to-process seam notes warn about.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

fn value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let identity = value(&args, "--identity")
        .ok_or_else(|| anyhow!("missing --identity <daemon --home dir>"))?;
    let world = value(&args, "--world").unwrap_or_else(|| "com.lait.issues".to_string());
    // Stable by default; test is offered only so a fixture channel can be
    // exercised through the identical path.
    let (channel, channel_name) = match value(&args, "--channel").as_deref() {
        None | Some("stable") => (lait::update::feed::Channel::Stable, "stable"),
        Some("test") => (lait::update::feed::Channel::Test, "test"),
        Some(other) => return Err(anyhow!("unknown channel {other}; use stable or test")),
    };

    // Resolve the identity home exactly as `lait daemon --home <identity>` does,
    // so the World lands where the daemon will look for it.
    let identity_dir = lait::config::Selection::for_identity(PathBuf::from(&identity))
        .identity_dir()
        .context("resolve daemon identity home")?;
    let worlds = lait::serve::head::installations_root(&identity_dir);

    let outcome = lait::update::world::check(&world, &worlds, channel)
        .with_context(|| format!("install {world} from the {channel_name} channel"))?;

    match outcome {
        lait::update::world::Outcome::Staged { version }
        | lait::update::world::Outcome::Current { version } => {
            println!(
                "world-host-install: installed {world} {version} into {}",
                worlds.display()
            );
            Ok(())
        }
        other => Err(anyhow!(
            "the {channel_name} channel did not install {world}: {other:?}"
        )),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("world-host-install: {error:#}");
        std::process::exit(1);
    }
}
