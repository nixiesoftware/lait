//! Install explicitly supplied signed World channels into an identity.
//!
//! This is test/developer transport, not a source of authority: the directory
//! is accepted only after its pointer, manifest, artifact size, digest, and
//! World declaration pass the production independent-install boundary.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

fn value(args: &[String], flag: &str) -> Result<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
        .ok_or_else(|| anyhow!("missing {flag}"))
}

fn values(args: &[String], flag: &str) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter_map(|(index, arg)| {
            (arg == flag)
                .then(|| args.get(index + 1).cloned())
                .flatten()
        })
        .collect()
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let channels = PathBuf::from(value(&args, "--channels")?);
    let identity = PathBuf::from(value(&args, "--identity")?);
    let worlds = values(&args, "--world");
    if worlds.is_empty() {
        bail!("at least one --world <reverse-domain-id> is required");
    }
    if !channels.is_dir() {
        bail!("signed channel directory is absent: {}", channels.display());
    }
    if !identity.is_absolute() {
        bail!("--identity must be an absolute path");
    }

    let encoded = std::fs::read_to_string(channels.join("pubkey.hex"))
        .context("read signed channel public key")?;
    let decoded = data_encoding::HEXLOWER
        .decode(encoded.trim().as_bytes())
        .context("decode signed channel public key")?;
    let pubkey: [u8; 32] = decoded
        .try_into()
        .map_err(|_| anyhow!("signed channel public key is not 32 bytes"))?;
    let installations = lait::serve::head::installations_root(&identity);

    for world in worlds {
        let outcome = lait::update::world::install_from_published_directory(
            &channels.join(&world),
            &[pubkey],
            &world,
            &lait::update::facts::offered(),
            &installations,
        )?;
        match outcome {
            lait::update::world::Outcome::Staged { version }
            | lait::update::world::Outcome::Current { version } => {
                println!("installed {world} {version}");
            }
            other => bail!("signed channel did not install {world}: {other:?}"),
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("world-channel-installer: {error:#}");
        std::process::exit(1);
    }
}
