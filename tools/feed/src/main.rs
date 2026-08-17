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
//! Every pointer is stamped with `published_at` at seal time. Clients keep the
//! newest stamp they have believed and refuse anything older, which is what
//! makes the one mutable object in the feed un-replayable. The stamp is not
//! optional going forward: once a client has seen one, an unstamped pointer is
//! refused, so publishing must never regress to a tool that omits it.
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
    Release {
        version: String,
        manifest: String,
        /// Unix seconds at seal time. The client's replay ratchet compares
        /// against it; see `lait::update::feed::check_freshness`.
        published_at: u64,
    },
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
        Some("world") => world(&args[1..]),
        _ => bail!(
            "usage: lait-feed <keygen|manifest|pointer|verify|world> ...\n\
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
    println!("public key (add to FEED_PUBKEYS_HEX): {}", device.as_str());
    println!(
        "rotating: add this key, ship that build, wait for adoption, then sign with this seed"
    );
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
/// optional (`--astrolabe <version>` names its bundle version) because lait can
/// still ship independently when a client platform job is unavailable.
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
        // One artifact per supported platform: NSIS on Windows, a signed DMG
        // on Apple silicon, and Flutter's relocatable bundle on Linux x64.
        // Each is included when the directory holds it; an absent platform is
        // a loud note rather than a refusal, because the jobs can succeed
        // independently and a publisher must be able to ship the half that
        // built. Refusing only when NONE exists keeps `--astrolabe` from
        // sealing a bundle version no artifact backs.
        let platforms: &[(&str, String)] = &[
            (
                "x86_64-pc-windows-msvc",
                format!("astrolabe-{astrolabe_version}-setup.exe"),
            ),
            (
                "aarch64-apple-darwin",
                format!("astrolabe-{astrolabe_version}.dmg"),
            ),
            (
                "x86_64-unknown-linux-gnu",
                format!("astrolabe-{astrolabe_version}-x86_64-unknown-linux-gnu.tar.gz"),
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
                "--astrolabe {astrolabe_version} given but {} holds no Astrolabe platform artifact",
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

    // Stamp the moment of publication inside the signed payload. This is what
    // makes the one mutable object in the feed un-replayable: a client records
    // the newest stamp it has believed and refuses anything older, so putting
    // an old correctly-signed pointer back in place becomes a refusal rather
    // than a silent freeze at that release.
    //
    // Taken from the clock rather than a flag, so no operator has to remember
    // it and no two publishes can share a value by mistake. A clock far behind
    // the last publish would emit a pointer clients refuse — an outage, not a
    // compromise, and cleared by publishing again with the clock fixed.
    let published_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the unix epoch")?
        .as_secs();

    let payload = PointerPayload::Release {
        version,
        manifest: manifest_url,
        published_at,
    };
    fs::write(&out, seal(&payload, &seed)?).with_context(|| format!("write {}", out.display()))?;
    println!(
        "pointer sealed to {} (published_at {published_at})",
        out.display()
    );
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

// ---------------------------------------------------------------------------
// The one-act World publish (SUB-23).
//
// A World's web head ships on its own cadence, so it gets its own release
// stream: `releases/worlds/<world>/<version>/` for the immutable half and
// `channels/worlds/<world>/<channel>` for the one mutable object. The layout
// is the product feed's, one level in, so every rule that already holds for a
// release holds here — pointer last, immutable releases, promotion is pointer
// motion — without a second contract to keep in step.
//
// **Compatibility is declared inside the bundle, not encoded in where it is
// filed.** An earlier cut keyed `artifacts[<world>][<runtime>]` by a
// fingerprint of the host, so an incompatible bundle was not found. That
// invalidated every published bundle whenever any unrelated host fact moved.
// A World now states named requirements in its own `world.json` — which rides
// inside the payload, under the artifact digest, under the feed signature —
// and the client decides after proving the bytes. One artifact per release,
// filed under `any`.

/// Publish a World's web head in one act, or promote one that is already
/// published.
///
/// Ordinary form: pack the bundle, seal a manifest for it, seal the channel
/// pointer that names it. Promotion (`--promote`) omits `--bundle` and seals
/// only a pointer at a version already on the host — the one-file rewrite that
/// makes a test release stable with no rebuild.
///
/// Nothing is uploaded here. Sealing is separable from publishing on purpose:
/// every refusal below happens before a byte reaches the host, and the upload
/// order that makes a feed safe (artifacts, then manifest, then the pointer
/// last) belongs to `ci/publish-world.sh`.
fn world(args: &[String]) -> Result<()> {
    let world = required(args, "--world")?;
    if world.is_empty()
        || !world.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
    {
        bail!("--world must be a reverse-domain id in lowercase ascii, got {world}");
    }
    let version = required(args, "--version")?;
    let version_v =
        semver::Version::parse(&version).with_context(|| format!("--version {version}"))?;
    let channel = required(args, "--channel")?;
    if channel != "stable" && channel != "test" {
        bail!("--channel must be stable or test, got {channel}");
    }
    // The same rule the product pointer enforces, for the same reason: a
    // stable follower must never be handed a prerelease.
    if channel == "stable" && !version_v.pre.is_empty() {
        bail!("the stable pointer never names a prerelease ({version})");
    }
    let base = required(args, "--base-url")?;
    let base = base.trim_end_matches('/');
    let seed = read_seed(&required(args, "--seed")?)?;
    let out = PathBuf::from(required(args, "--out")?);
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;

    let release_prefix = format!("releases/worlds/{world}/{version}");
    let manifest_url = format!("{base}/{release_prefix}/manifest.json");
    let manifest_path = out.join("manifest.json");
    let pointer_path = out.join("pointer");

    if arg(args, "--promote").is_none() {
        let bundle = PathBuf::from(required(args, "--bundle")?);
        if !bundle.is_dir() {
            bail!("--bundle {} is not a directory", bundle.display());
        }
        // The declaration is what makes a directory of files a World, and it
        // is checked here rather than discovered by a machine that already
        // downloaded it. A publisher learns at publish time.
        let declared = fs::read(bundle.join("world.json")).with_context(|| {
            format!(
                "--bundle {} carries no world.json at its root — a World must declare what it is \
                 and how to reach it",
                bundle.display()
            )
        })?;
        let declaration = world_interface::manifest::WorldManifest::parse(&declared)
            .map_err(|error| anyhow!("{error}"))?;
        if declaration.id != world {
            bail!(
                "publishing {world} but the bundle declares itself {} — a World may not answer \
                 for another",
                declaration.id
            );
        }
        if declaration.version != version {
            bail!(
                "publishing {version} but the bundle declares {} — the declaration is what a \
                 client believes",
                declaration.version
            );
        }
        let name = format!("world-{world}-{version}.tar.gz");
        let archive = out.join(&name);
        pack_world(&bundle, &archive, &format!("world-{world}-{version}"))?;
        let (digest, size) = hash_file(&archive)?;

        let mut targets = BTreeMap::new();
        targets.insert(
            "any".to_string(),
            Artifact {
                url: format!("{base}/{release_prefix}/{name}"),
                blake3: digest,
                size,
            },
        );
        let mut bundles = BTreeMap::new();
        bundles.insert(world.clone(), version.clone());
        let mut artifacts = BTreeMap::new();
        artifacts.insert(world.clone(), targets);

        let manifest = Manifest {
            version: version.clone(),
            min_supported: None,
            bundles,
            artifacts,
        };
        fs::write(&manifest_path, seal(&manifest, &seed)?)
            .with_context(|| format!("write {}", manifest_path.display()))?;
        let requires = if declaration.requires.is_empty() {
            "no host requirements".to_string()
        } else {
            declaration
                .requires
                .iter()
                .map(|r| format!("{} {}", r.name, r.range))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "packed {} ({size} bytes) and sealed its manifest — {requires}",
            archive.display()
        );
    } else if !manifest_path.is_file() {
        bail!(
            "--promote needs the sealed manifest of the release being promoted at {}; \
             fetch it from {manifest_url} first",
            manifest_path.display()
        );
    }

    // The pointer is sealed against the manifest it names, opened with the
    // key that is about to sign it — the same last gate the product pointer
    // takes, so a promotion cannot name a release this key never sealed.
    let pubkey = pubkey_of_seed(&seed)?;
    let sealed = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let payload = open(&sealed, &pubkey).context("World manifest envelope")?;
    let manifest: serde_json::Value = serde_json::from_slice(&payload)?;
    if manifest["version"].as_str() != Some(version.as_str()) {
        bail!(
            "pointer names {version} but the manifest says {}",
            manifest["version"]
        );
    }
    if manifest["bundles"][&world].as_str() != Some(version.as_str()) {
        bail!("the manifest carries no {world} bundle at {version}");
    }

    let published_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the unix epoch")?
        .as_secs();
    let pointer = PointerPayload::Release {
        version: version.clone(),
        manifest: manifest_url,
        published_at,
    };
    fs::write(&pointer_path, seal(&pointer, &seed)?)
        .with_context(|| format!("write {}", pointer_path.display()))?;

    println!(
        "pointer sealed to {} (published_at {published_at})",
        pointer_path.display()
    );
    println!("upload, in this order — the pointer LAST:");
    println!("  {release_prefix}/world-{world}-{version}.tar.gz");
    println!("  {release_prefix}/manifest.json");
    println!("  channels/worlds/{world}/{channel}");
    Ok(())
}

/// Pack a built web head into the artifact shape a client extracts: gzip'd
/// tar with one root directory, every path beneath it.
///
/// Deterministic on purpose — sorted entries, zeroed mtimes and ownership,
/// and a root directory named by the caller rather than inferred from the
/// output path — so republishing an unchanged bundle produces an unchanged
/// digest, and a digest that moved means the content moved. Deriving the root
/// from the file name was the first version of this, and it made two writes of
/// one bundle differ; the test below is what said so.
fn pack_world(bundle: &Path, archive: &Path, root: &str) -> Result<()> {
    let mut paths = Vec::new();
    collect(bundle, bundle, &mut paths)?;
    if paths.is_empty() {
        bail!("--bundle {} holds no files", bundle.display());
    }
    paths.sort();

    let file =
        fs::File::create(archive).with_context(|| format!("create {}", archive.display()))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut builder = tar::Builder::new(encoder);
    for relative in &paths {
        let source = bundle.join(relative);
        let contents = fs::read(&source).with_context(|| format!("read {}", source.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{root}/{relative}"), &contents[..])
            .with_context(|| format!("append {relative}"))?;
    }
    builder
        .into_inner()
        .context("seal the world tar")?
        .finish()
        .context("seal the world gzip")?;
    Ok(())
}

/// Every file under `dir`, as `/`-separated paths relative to `base`.
fn collect(base: &Path, dir: &Path, into: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(base, &path, into)?;
        } else {
            let relative = path
                .strip_prefix(base)
                .expect("a walked path is under its base");
            into.push(
                relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(dir: &Path) -> String {
        let seed = dir.join("seed");
        keygen(&["--out".into(), seed.display().to_string()]).expect("a seed is minted");
        seed.display().to_string()
    }

    fn bundle(dir: &Path) -> String {
        bundle_declaring(dir, "com.lait.issues", "0.1.0")
    }

    fn bundle_declaring(dir: &Path, id: &str, version: &str) -> String {
        let bundle = dir.join("bundle");
        fs::create_dir_all(bundle.join("assets")).expect("a bundle tree");
        fs::write(bundle.join("index.html"), b"<html>head</html>").expect("an entry document");
        fs::write(bundle.join("assets/app.css"), b"body{}").expect("an asset");
        let declaration = serde_json::json!({
            "format": 1,
            "id": id,
            "mount": "issues",
            "version": version,
            "requires": [{ "name": "lait.control", "range": ">=13, <14" }],
            "launch": [{ "id": "app", "present": "primary",
                         "target": { "type": "web", "path": "/" } }],
        });
        fs::write(bundle.join("world.json"), declaration.to_string()).expect("a declaration");
        bundle.display().to_string()
    }

    fn publish(dir: &Path, args: &[&str]) -> Result<()> {
        let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        world(&owned)
    }

    /// Republishing an unchanged bundle must produce an unchanged digest, or
    /// a digest that moved stops meaning the content moved — and every
    /// verification downstream is comparing noise.
    #[test]
    fn packing_the_same_bundle_twice_produces_the_same_bytes() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let source = bundle(dir.path());
        let first = dir.path().join("first.tar.gz");
        let second = dir.path().join("second.tar.gz");
        pack_world(Path::new(&source), &first, "world-com.lait.issues-0.1.0")
            .expect("the first pack");
        pack_world(Path::new(&source), &second, "world-com.lait.issues-0.1.0")
            .expect("the second pack");
        assert_eq!(
            hash_file(&first).expect("first digest").0,
            hash_file(&second).expect("second digest").0,
            "packing is not deterministic, so a republish looks like a content change"
        );
    }

    #[test]
    fn a_world_publish_seals_a_manifest_naming_one_artifact_for_the_release() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let seed = seeded(dir.path());
        let source = bundle(dir.path());
        let out = dir.path().join("out");
        publish(
            dir.path(),
            &[
                "--world",
                "com.lait.issues",
                "--version",
                "0.1.0",
                "--runtime",
                "rt-abc",
                "--bundle",
                &source,
                "--channel",
                "test",
                "--base-url",
                "https://feed.example",
                "--seed",
                &seed,
                "--out",
                &out.display().to_string(),
            ],
        )
        .expect("the World publishes");

        let sealed = fs::read_to_string(out.join("manifest.json")).expect("the sealed manifest");
        let pubkey = pubkey_of_seed(&read_seed(&seed).expect("the seed")).expect("its public key");
        let payload = open(&sealed, &pubkey).expect("the manifest opens under its own key");
        let manifest: serde_json::Value = serde_json::from_slice(&payload).expect("it parses");
        assert_eq!(manifest["bundles"]["com.lait.issues"], "0.1.0");
        // One artifact per release. What a bundle runs against is stated in
        // its own declaration, inside the payload and under the digest —
        // never encoded in where the artifact is filed.
        assert!(
            manifest["artifacts"]["com.lait.issues"]["any"]["size"]
                .as_u64()
                .is_some_and(|size| size > 0),
            "the manifest does not name one artifact for the release: {manifest}"
        );
    }

    #[test]
    fn a_stable_channel_never_takes_a_prerelease() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let seed = seeded(dir.path());
        let source = bundle_declaring(dir.path(), "com.lait.issues", "0.2.0-test.1");
        let error = publish(
            dir.path(),
            &[
                "--world",
                "com.lait.issues",
                "--version",
                "0.2.0-test.1",
                "--runtime",
                "rt-abc",
                "--bundle",
                &source,
                "--channel",
                "stable",
                "--base-url",
                "https://feed.example",
                "--seed",
                &seed,
                "--out",
                &dir.path().join("out").display().to_string(),
            ],
        )
        .expect_err("a prerelease on stable must refuse")
        .to_string();
        assert!(error.contains("never names a prerelease"), "{error}");
    }

    #[test]
    fn a_world_id_that_is_not_a_reverse_domain_is_refused() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let seed = seeded(dir.path());
        let error = publish(
            dir.path(),
            &[
                "--world",
                "Com Lait Issues",
                "--version",
                "0.1.0",
                "--channel",
                "test",
                "--base-url",
                "https://feed.example",
                "--seed",
                &seed,
                "--out",
                &dir.path().join("out").display().to_string(),
            ],
        )
        .expect_err("a malformed World id must refuse")
        .to_string();
        assert!(error.contains("reverse-domain id"), "{error}");
    }

    /// A directory of files is not a World until it says what it is. Caught
    /// at publish time, where a publisher can fix it, rather than by a machine
    /// that already downloaded it.
    #[test]
    fn a_bundle_with_no_declaration_is_refused_at_publish_time() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let seed = seeded(dir.path());
        let bare = dir.path().join("bare");
        fs::create_dir_all(&bare).expect("a bundle tree");
        fs::write(bare.join("index.html"), b"<html/>").expect("an entry document");
        let error = publish(
            dir.path(),
            &[
                "--world",
                "com.lait.issues",
                "--version",
                "0.1.0",
                "--bundle",
                &bare.display().to_string(),
                "--channel",
                "test",
                "--base-url",
                "https://feed.example",
                "--seed",
                &seed,
                "--out",
                &dir.path().join("out").display().to_string(),
            ],
        )
        .expect_err("an undeclared bundle must refuse")
        .to_string();
        assert!(error.contains("world.json"), "{error}");
    }

    #[test]
    fn a_bundle_that_declares_another_world_or_version_is_refused() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let seed = seeded(dir.path());
        let source = bundle_declaring(dir.path(), "com.someone.else", "0.1.0");
        let error = publish(
            dir.path(),
            &[
                "--world",
                "com.lait.issues",
                "--version",
                "0.1.0",
                "--bundle",
                &source,
                "--channel",
                "test",
                "--base-url",
                "https://feed.example",
                "--seed",
                &seed,
                "--out",
                &dir.path().join("out").display().to_string(),
            ],
        )
        .expect_err("a mismatched declaration must refuse")
        .to_string();
        assert!(error.contains("may not answer for another"), "{error}");
    }

    /// Promotion may only ever name a release this key sealed. Without the
    /// manifest on hand there is nothing to prove that against, and sealing a
    /// pointer anyway would let a promotion name bytes nobody published.
    #[test]
    fn promoting_without_the_sealed_manifest_is_refused_rather_than_guessed() {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let seed = seeded(dir.path());
        let error = publish(
            dir.path(),
            &[
                "--world",
                "com.lait.issues",
                "--version",
                "0.1.0",
                "--channel",
                "stable",
                "--promote",
                "yes",
                "--base-url",
                "https://feed.example",
                "--seed",
                &seed,
                "--out",
                &dir.path().join("out").display().to_string(),
            ],
        )
        .expect_err("a promotion with no manifest must refuse")
        .to_string();
        assert!(error.contains("needs the sealed manifest"), "{error}");
    }
}
