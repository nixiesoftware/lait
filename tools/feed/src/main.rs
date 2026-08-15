//! The publish half of the release feed (SUB-13).
//!
//! The feed is proven, not trusted: everything an installed machine believes —
//! a channel pointer, a release manifest, a relocation — is a signed envelope
//! verified against a key pinned in the binary. This tool is where those
//! envelopes are sealed, and where the publish-time rules that must fail
//! *before* a pointer moves are enforced:
//!
//! - a pointer's floor may never exceed the version it points at (an
//!   unsatisfiable floor would force every installed machine forever), and
//! - a stable pointer may never name a prerelease.
//!
//! Subcommands:
//!
//! ```text
//! lait-feed keygen  --out <seed-file>
//! lait-feed manifest --version <v> --base-url <url> --artifacts-dir <dir>
//!                    --seed <file> --out <file> [--floor <v>] [--astrolabe <v>]
//! lait-feed pointer --channel <stable|test> --version <v> --manifest-url <url>
//!                   --manifest <file> --seed <file> --out <file>
//! lait-feed verify  --pubkey <hex> --file <file>
//! ```
//!
//! The envelope and payload shapes are mirrored by `lait::update::feed`, which
//! opens what this seals; its tests round-trip both directions.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Every target the release pipeline ships, with the archive extension
/// cargo-dist gives it. The publish step refuses an artifacts directory that is
/// missing any of these: a release that quietly dropped a platform would strand
/// that platform's installed base on the previous version with no error
/// anywhere.
const LAIT_TARGETS: &[(&str, &str)] = &[
    ("x86_64-pc-windows-msvc", "zip"),
    ("aarch64-apple-darwin", "tar.gz"),
    ("x86_64-apple-darwin", "tar.gz"),
    ("aarch64-unknown-linux-gnu", "tar.gz"),
    ("x86_64-unknown-linux-gnu", "tar.gz"),
];

#[derive(Serialize)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Serialize)]
struct Artifact {
    url: String,
    blake3: String,
    size: u64,
}

#[derive(Serialize)]
struct Manifest {
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_supported: Option<String>,
    bundles: BTreeMap<String, String>,
    artifacts: BTreeMap<String, BTreeMap<String, Artifact>>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PointerPayload {
    Release { version: String, manifest: String },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lait-feed: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => keygen(&args[1..]),
        Some("manifest") => manifest(&args[1..]),
        Some("pointer") => pointer(&args[1..]),
        Some("verify") => verify(&args[1..]),
        _ => bail!(
            "usage: lait-feed <keygen|manifest|pointer|verify> ...\n\
             see the module doc in tools/feed/src/main.rs"
        ),
    }
}

/// `--flag value` argument lookup; every flag here is required unless the
/// caller falls back explicitly.
fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn required(args: &[String], flag: &str) -> Result<String> {
    arg(args, flag).ok_or_else(|| anyhow!("missing {flag}"))
}

/// Mint a feed signing seed and print the public key to pin in the binary.
///
/// The seed file is the release-signing authority: whoever holds it can make
/// every installed machine believe a release. It is written where the caller
/// says and never anywhere else; keeping it off build machines and out of the
/// repository is the caller's charge.
fn keygen(args: &[String]) -> Result<()> {
    let out = PathBuf::from(required(args, "--out")?);
    if out.exists() {
        bail!(
            "{} already exists; refusing to overwrite a signing key",
            out.display()
        );
    }
    let seed = mechanics::actor::random_seed().map_err(|e| anyhow!("entropy: {e:?}"))?;
    let device = mechanics::actor::device_from_seed(&seed);
    fs::write(&out, data_encoding::HEXLOWER.encode(&seed))
        .with_context(|| format!("write {}", out.display()))?;
    println!("seed written to {}", out.display());
    println!("public key (pin as FEED_PUBKEY_HEX): {}", device.as_str());
    Ok(())
}

fn read_seed(path: &str) -> Result<[u8; 32]> {
    let hex = fs::read_to_string(path).with_context(|| format!("read seed {path}"))?;
    let bytes = data_encoding::HEXLOWER
        .decode(hex.trim().as_bytes())
        .map_err(|e| anyhow!("seed {path} is not lowercase hex: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("seed {path} is not 32 bytes"))
}

fn seal(payload: &impl Serialize, seed: &[u8; 32]) -> Result<String> {
    let bytes = serde_json::to_vec(payload)?;
    let signature = mechanics::actor::sign_detached(seed, &bytes);
    let envelope = Envelope {
        payload: data_encoding::BASE64.encode(&bytes),
        signature: data_encoding::BASE64.encode(&signature),
    };
    Ok(serde_json::to_string(&envelope)?)
}

/// Open an envelope produced by [`seal`] (or refuse it). Duplicated from
/// `lait::update::feed` rather than imported so this tool never depends on the
/// application crate; the shared shape is held together by the round-trip test
/// there, which seals here and opens there.
fn open(text: &str, pubkey: &[u8; 32]) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(text).context("envelope is not JSON")?;
    let payload = value["payload"]
        .as_str()
        .ok_or_else(|| anyhow!("envelope has no payload"))?;
    let signature = value["signature"]
        .as_str()
        .ok_or_else(|| anyhow!("envelope has no signature"))?;
    let payload = data_encoding::BASE64
        .decode(payload.as_bytes())
        .context("payload base64")?;
    let signature: [u8; 64] = data_encoding::BASE64
        .decode(signature.as_bytes())
        .context("signature base64")?
        .try_into()
        .map_err(|_| anyhow!("signature is not 64 bytes"))?;
    if !mechanics::actor::verify_detached(pubkey, &payload, &signature) {
        bail!("signature verification failed");
    }
    Ok(payload)
}

fn pubkey_of_seed(seed: &[u8; 32]) -> Result<[u8; 32]> {
    let device = mechanics::actor::device_from_seed(seed);
    let bytes = data_encoding::HEXLOWER
        .decode(device.as_str().as_bytes())
        .map_err(|e| anyhow!("device key hex: {e}"))?;
    bytes.try_into().map_err(|_| anyhow!("device key length"))
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok((
        blake3::hash(&bytes).to_hex().to_string(),
        bytes.len() as u64,
    ))
}

/// Build and sign the release manifest from a directory of built artifacts.
///
/// Refuses a directory missing any lait target. The astrolabe installer is
/// optional (`--astrolabe <version>` names its bundle version; the installer is
/// expected as `astrolabe-<version>-setup.exe`) because the client ships
/// Windows-first while lait ships everywhere.
fn manifest(args: &[String]) -> Result<()> {
    let version = required(args, "--version")?;
    semver::Version::parse(&version).with_context(|| format!("--version {version}"))?;
    let base = required(args, "--base-url")?;
    let base = base.trim_end_matches('/');
    let dir = PathBuf::from(required(args, "--artifacts-dir")?);
    let seed = read_seed(&required(args, "--seed")?)?;
    let out = PathBuf::from(required(args, "--out")?);
    let floor = arg(args, "--floor");
    if let Some(floor) = &floor {
        let floor_v = semver::Version::parse(floor).with_context(|| format!("--floor {floor}"))?;
        let version_v = semver::Version::parse(&version)?;
        // The same rule the pointer enforces, caught at the earliest moment it
        // is checkable: a floor above the release it ships in is unsatisfiable.
        if floor_v > version_v {
            bail!("floor {floor} exceeds the release version {version}: unsatisfiable");
        }
    }

    let mut bundles = BTreeMap::new();
    let mut artifacts: BTreeMap<String, BTreeMap<String, Artifact>> = BTreeMap::new();

    bundles.insert("lait".to_string(), version.clone());
    let mut lait = BTreeMap::new();
    for (target, ext) in LAIT_TARGETS {
        let name = format!("lait-{target}.{ext}");
        let path = dir.join(&name);
        if !path.is_file() {
            bail!(
                "missing artifact {} — a release that drops a platform strands it silently",
                path.display()
            );
        }
        let (digest, size) = hash_file(&path)?;
        lait.insert(
            target.to_string(),
            Artifact {
                url: format!("{base}/releases/{version}/{name}"),
                blake3: digest,
                size,
            },
        );
    }
    artifacts.insert("lait".to_string(), lait);

    if let Some(astrolabe_version) = arg(args, "--astrolabe") {
        // One installer per platform: the NSIS setup.exe for Windows, the
        // signed DMG for macOS (Apple silicon; the bundle is arm64-only by
        // the AppInfo.xcconfig ARCHS decision). Each is included when the
        // directory holds it; an absent platform is a loud note rather than
        // a refusal, because the two installer jobs can succeed independently
        // and a publisher must be able to ship the half that built. Refusing
        // only when NEITHER exists keeps `--astrolabe` from sealing a bundle
        // version no artifact backs. Once both platforms have shipped a
        // release, tightening this to "both or refuse" is the LAIT_TARGETS
        // rule and worth doing.
        let platforms: &[(&str, String)] = &[
            (
                "x86_64-pc-windows-msvc",
                format!("astrolabe-{astrolabe_version}-setup.exe"),
            ),
            (
                "aarch64-apple-darwin",
                format!("astrolabe-{astrolabe_version}.dmg"),
            ),
        ];
        let mut astrolabe = BTreeMap::new();
        for (target, name) in platforms {
            let path = dir.join(name);
            if !path.is_file() {
                eprintln!("lait-feed: NOTE — no {name}; publishing astrolabe without {target}");
                continue;
            }
            let (digest, size) = hash_file(&path)?;
            astrolabe.insert(
                target.to_string(),
                Artifact {
                    url: format!("{base}/releases/{version}/{name}"),
                    blake3: digest,
                    size,
                },
            );
        }
        if astrolabe.is_empty() {
            bail!(
                "--astrolabe {astrolabe_version} given but {} holds neither \
                 astrolabe-{astrolabe_version}-setup.exe nor astrolabe-{astrolabe_version}.dmg",
                dir.display()
            );
        }
        bundles.insert("astrolabe".to_string(), astrolabe_version);
        artifacts.insert("astrolabe".to_string(), astrolabe);
    }

    let manifest = Manifest {
        version,
        min_supported: floor,
        bundles,
        artifacts,
    };
    fs::write(&out, seal(&manifest, &seed)?).with_context(|| format!("write {}", out.display()))?;
    println!("manifest sealed to {}", out.display());
    Ok(())
}

/// Build and sign a channel pointer, enforcing the rules that must hold before
/// a pointer may move: the named manifest exists, verifies, agrees on the
/// version, carries no unsatisfiable floor, and a stable pointer names no
/// prerelease.
fn pointer(args: &[String]) -> Result<()> {
    let channel = required(args, "--channel")?;
    if channel != "stable" && channel != "test" {
        bail!("--channel must be stable or test, got {channel}");
    }
    let version = required(args, "--version")?;
    let version_v =
        semver::Version::parse(&version).with_context(|| format!("--version {version}"))?;
    if channel == "stable" && !version_v.pre.is_empty() {
        bail!("the stable pointer never names a prerelease ({version})");
    }
    let manifest_url = required(args, "--manifest-url")?;
    let manifest_file = required(args, "--manifest")?;
    let seed = read_seed(&required(args, "--seed")?)?;
    let out = PathBuf::from(required(args, "--out")?);

    // Prove the manifest we are about to point at: right key, right version,
    // satisfiable floor. This is the last gate before the one mutable object
    // in the feed moves.
    let pubkey = pubkey_of_seed(&seed)?;
    let manifest_text = fs::read_to_string(&manifest_file)
        .with_context(|| format!("read manifest {manifest_file}"))?;
    let payload = open(&manifest_text, &pubkey).context("manifest envelope")?;
    let manifest: serde_json::Value = serde_json::from_slice(&payload)?;
    let manifest_version = manifest["version"]
        .as_str()
        .ok_or_else(|| anyhow!("manifest has no version"))?;
    if manifest_version != version {
        bail!("pointer names {version} but the manifest says {manifest_version}");
    }
    if let Some(floor) = manifest["min_supported"].as_str() {
        let floor_v = semver::Version::parse(floor).context("manifest min_supported")?;
        if floor_v > version_v {
            bail!("floor {floor} exceeds {version}: unsatisfiable, refusing to move the pointer");
        }
    }

    let payload = PointerPayload::Release {
        version,
        manifest: manifest_url,
    };
    fs::write(&out, seal(&payload, &seed)?).with_context(|| format!("write {}", out.display()))?;
    println!("pointer sealed to {}", out.display());
    Ok(())
}

/// Open an envelope against a public key and print its payload — the check a
/// publish script runs after upload, against the bytes the host actually
/// serves.
fn verify(args: &[String]) -> Result<()> {
    let pubkey_hex = required(args, "--pubkey")?;
    let pubkey: [u8; 32] = data_encoding::HEXLOWER
        .decode(pubkey_hex.trim().as_bytes())
        .context("--pubkey hex")?
        .try_into()
        .map_err(|_| anyhow!("--pubkey is not 32 bytes"))?;
    let file = required(args, "--file")?;
    let text = fs::read_to_string(&file).with_context(|| format!("read {file}"))?;
    let payload = open(&text, &pubkey)?;
    println!("{}", String::from_utf8_lossy(&payload));
    Ok(())
}
