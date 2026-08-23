//! Fetching a World's web head from its own channel (SUB-22).
//!
//! The other half of [`super::runtime`] and `serve::head`: those decide which
//! bundle a head *serves*, this one puts a bundle there. A World ships on its
//! own cadence, so it has its own mutable object —
//! `channels/worlds/<world>/<channel>` — resolved by exactly the rules the
//! product's channel is resolved by, because [`feed::resolve_pointer_with`] is
//! the same function. Signature, one-hop relocation, freshness ratchet, and
//! the stable-never-prerelease refusal all come along; nothing about a World
//! gets a second, weaker contract.
//!
//! ## Compatibility is declared, fetched, and then decided
//!
//! An earlier cut keyed artifacts by a fingerprint of this build, so an
//! incompatible bundle was *not found*. That was wrong in a way only a
//! publisher feels: the fingerprint covered every schema and every World's
//! implementation, so a change touching none of a publisher's dependencies
//! still invalidated their bundle. A World now declares named requirements in
//! its own `world.json` — `lait.control` at `>=13, <14` — and this path
//! fetches, proves the bytes, reads the declaration, and *then* decides.
//!
//! The cost is a download that may not be activated; the gain is that a
//! bundle survives every change it does not name. What is never traded is the
//! order: bytes are proven against the signed manifest before anything is
//! read out of them, and a bundle whose requirements are unmet is refused by
//! name with whatever was already in place still serving.
//!
//! ## Staging is where a bundle is proven
//!
//! Bytes are verified against the signed manifest — size then digest — before
//! anything is extracted, and the tree lands in a scratch directory that is
//! renamed into place only once it is whole and its requirements are met.
//! `serve::head` therefore serves a directory it did not verify itself, and is
//! right to: a directory present under a World's name is one that was proven
//! when it landed, by this.
//!
//! Each World gets its own directory. The first cut keyed staging by runtime
//! alone, so a second World overwrote the first and both re-downloaded on
//! every period — a collision that only appears once more than one World is
//! published, which is to say after it would have been expensive.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{feed, tree};

/// What a check of one World's channel came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A bundle was fetched, proven, met its requirements, and is in place.
    Staged {
        /// The bundle version now staged for this World.
        version: String,
    },
    /// The channel's release is already staged, so nothing was fetched.
    /// Re-downloading it every period would spend bytes to learn nothing.
    Current {
        /// The version already in place.
        version: String,
    },
    /// The bundle was proven and declares requirements this build does not
    /// meet. Whatever selected release was already in place keeps serving, and
    /// each unmet requirement is named.
    ///
    /// Not an error: a publisher shipping for a newer host is an ordinary
    /// state of the world, and the machine is not broken by it.
    Unmet {
        /// The version that could not be activated.
        version: String,
        /// Every requirement this build failed, each saying which kind of
        /// "no" it is.
        why: Vec<String>,
    },
    /// The channel resolved and its release carries no bundle for this World.
    NothingPublished {
        /// The release the channel names, which holds nothing for us.
        version: String,
    },
}

/// Observable phases of an explicitly requested World installation.
///
/// These are deliberately mechanical and bounded. They let the native client
/// say that an install is moving without making any phase authoritative: only
/// the selected immutable release record proves completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallProgress {
    Resolving,
    Downloading { received: u64, total: u64 },
    Verifying,
    Installing,
}

/// Which immutable release the current pointer selects.
///
/// Beside, because everything inside that directory may be *served*: a marker
/// within it would be reachable at a URL, and a served tree must hold only
/// what its publisher put there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledBundle {
    /// The World this bundle belongs to.
    pub world: String,
    /// The bundle version.
    pub version: String,
    /// Digest of the signed bundle artifact that produced this release.
    pub digest: String,
    /// How many files it holds.
    pub files: usize,
}

/// The directory a World's payloads live under, one per World.
pub fn world_root(worlds: &Path, world: &str) -> PathBuf {
    worlds.join(world)
}

/// One immutable release directory.
pub fn release_dir(worlds: &Path, world: &str, version: &str) -> Option<PathBuf> {
    semver::Version::parse(version).ok()?;
    Some(world_root(worlds, world).join("releases").join(version))
}

fn record_path(worlds: &Path, world: &str) -> PathBuf {
    world_root(worlds, world).join("selected.json")
}

fn release_record_path(worlds: &Path, world: &str, version: &str) -> Option<PathBuf> {
    semver::Version::parse(version).ok()?;
    Some(
        world_root(worlds, world)
            .join("records")
            .join(format!("{version}.json")),
    )
}

fn installed(worlds: &Path, world: &str, version: &str) -> Option<InstalledBundle> {
    let bytes = std::fs::read(release_record_path(worlds, world, version)?).ok()?;
    let release: InstalledBundle = serde_json::from_slice(&bytes).ok()?;
    if release.world != world
        || release.version != version
        || !release_dir(worlds, world, version)?.is_dir()
    {
        return None;
    }
    Some(release)
}

fn write_record(path: &Path, record: &InstalledBundle) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("World release record has no parent"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create World release records at {}", parent.display()))?;
    let encoded = serde_json::to_vec_pretty(record).context("encode the World release record")?;
    let staging = parent.join(tree::scratch_name("record.tmp-"));
    std::fs::write(&staging, encoded).context("write the World release record")?;
    mechanics::secretfs::persist_replace(&staging, path)
        .with_context(|| format!("seal World release record at {}", path.display()))
}

fn select(worlds: &Path, release: &InstalledBundle) -> Result<()> {
    write_record(&record_path(worlds, &release.world), release)
}

/// What is staged for a World, when anything is.
pub fn selected(worlds: &Path, world: &str) -> Option<InstalledBundle> {
    let bytes = std::fs::read(record_path(worlds, world)).ok()?;
    let staged: InstalledBundle = serde_json::from_slice(&bytes).ok()?;
    if staged.world != world || !release_dir(worlds, world, &staged.version)?.is_dir() {
        return None;
    }
    Some(staged)
}

/// The immutable release directory selected for future launches and heads.
pub fn active_dir(worlds: &Path, world: &str) -> Option<PathBuf> {
    let staged = selected(worlds, world)?;
    release_dir(worlds, world, &staged.version)
}

fn standing_path(worlds: &Path, world: &str) -> PathBuf {
    world_root(worlds, world).join("standing.json")
}

/// What this machine last learned about one World's channel.
///
/// Beside the staged bundle rather than inside it, for the same reason
/// `selected.json` is beside it: everything under a release may be *served*,
/// and a served tree holds only what its publisher put there.
///
/// This exists because the daemon's period is hours long and a World is
/// published in seconds. Between the two, a machine is behind and has no way
/// to say so — the outcome of a check was written to a log and nowhere a
/// surface could read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    /// The bundle version serving now. `None` means no valid release is selected.
    pub serving: Option<String>,
    /// The version this World's channel named at the last completed check.
    /// `None` when the channel could not be asked or named nothing for us.
    pub channel: Option<String>,
    /// Unix seconds of that check.
    pub checked_at: u64,
    /// Set when the channel's release is proven and this build cannot run it,
    /// naming every unmet requirement. Whatever was already in place keeps
    /// serving; this is a refusal, not a fault.
    #[serde(default)]
    pub unmet: Option<Vec<String>>,
}

impl Standing {
    /// True only when the channel is *known* to hold a bundle this machine is
    /// not serving, and this build could run it.
    ///
    /// Every uncertainty answers false. A machine that has never checked is
    /// not behind, a channel that could not be asked is not behind, and a
    /// bundle this build cannot run is not something a person can act on —
    /// offering an update that would be refused on arrival is worse than
    /// offering none.
    pub fn behind(&self) -> bool {
        if self.unmet.is_some() {
            return false;
        }
        match (&self.channel, &self.serving) {
            (Some(channel), Some(serving)) => channel != serving,
            (Some(_), None) => true,
            _ => false,
        }
    }
}

/// What this machine last learned about a World, when it has ever asked.
pub fn standing(worlds: &Path, world: &str) -> Option<Standing> {
    let bytes = std::fs::read(standing_path(worlds, world)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Record what a check came to, so a surface can read it later.
///
/// Best-effort by design: a standing that could not be written is a surface
/// that says nothing, which is the same thing it says before the first check.
/// Failing the check over it would trade a working update for a missing note.
pub fn note(worlds: &Path, world: &str, outcome: &Outcome, now: u64) {
    let serving = selected(worlds, world).map(|bundle| bundle.version);
    let standing = match outcome {
        Outcome::Staged { version } | Outcome::Current { version } => Standing {
            serving: Some(version.clone()),
            channel: Some(version.clone()),
            checked_at: now,
            unmet: None,
        },
        Outcome::Unmet { version, why } => Standing {
            serving,
            channel: Some(version.clone()),
            checked_at: now,
            unmet: Some(why.clone()),
        },
        // The channel answered and holds nothing for this World. Not a
        // failure, and emphatically not "behind": there is nothing to be
        // behind.
        Outcome::NothingPublished { .. } => Standing {
            serving,
            channel: None,
            checked_at: now,
            unmet: None,
        },
    };
    let path = standing_path(worlds, world);
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(&standing) {
        let _ = std::fs::write(&path, bytes);
    }
}

/// The pointer URL of a World's channel.
pub fn pointer_url(base: &str, world: &str, channel: feed::Channel) -> String {
    format!(
        "{}/channels/worlds/{world}/{}",
        base.trim_end_matches('/'),
        channel.as_str()
    )
}

/// The artifact key a World's payload is published under.
///
/// The native artifact for this exact build target.
pub const ARTIFACT: &str = env!("LAIT_TARGET");

/// Fetch and stage this World's payload, if the channel holds one this node
/// does not already have and can run.
///
/// Blocking, and policy-free: whether to check at all is the caller's
/// decision, and the fetch is injected for the same reason every other
/// function on this path injects one.
pub fn stage_bundle_with<F>(
    fetch: F,
    resolved: &feed::Resolved,
    world: &str,
    offers: &std::collections::BTreeMap<String, semver::Version>,
    worlds: &Path,
) -> Result<Outcome>
where
    F: Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure>,
{
    stage_bundle_with_progress(fetch, resolved, world, offers, worlds, |_| {})
}

/// [`stage_bundle_with`] with observable install phases.
pub fn stage_bundle_with_progress<F, P>(
    fetch: F,
    resolved: &feed::Resolved,
    world: &str,
    offers: &std::collections::BTreeMap<String, semver::Version>,
    worlds: &Path,
    progress: P,
) -> Result<Outcome>
where
    F: Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure>,
    P: Fn(InstallProgress),
{
    let version = resolved
        .manifest
        .bundles
        .get(world)
        .cloned()
        .unwrap_or_else(|| resolved.version.to_string());

    let Some(artifact) = resolved
        .manifest
        .artifacts
        .get(world)
        .and_then(|keyed| keyed.get(ARTIFACT))
    else {
        return Ok(Outcome::NothingPublished { version });
    };

    if let Some(bundle) = selected(worlds, world).filter(|bundle| bundle.version == version) {
        if bundle.digest.eq_ignore_ascii_case(&artifact.blake3) {
            return Ok(Outcome::Current { version });
        }
        bail!(
            "World {world} version {version} was already installed from digest {} and the channel now names {}",
            bundle.digest,
            artifact.blake3
        );
    }

    if let Some(bundle) = installed(worlds, world, &version) {
        if !bundle.digest.eq_ignore_ascii_case(&artifact.blake3) {
            bail!(
                "World {world} version {version} is immutable at digest {} and the channel now names {}",
                bundle.digest,
                artifact.blake3
            );
        }
        progress(InstallProgress::Installing);
        select(worlds, &bundle)?;
        return Ok(Outcome::Staged { version });
    }

    if release_dir(worlds, world, &version).is_some_and(|path| path.exists()) {
        bail!(
            "World {world} version {version} has an unrecorded release directory; refusing to replace bytes that a running generation may use"
        );
    }

    let bytes = fetch(&artifact.url, artifact.size)
        .map_err(|error| anyhow!("world bundle download: {error}"))?;
    progress(InstallProgress::Verifying);
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.size {
        bail!(
            "world bundle size mismatch: manifest says {} bytes, got {}",
            artifact.size,
            bytes.len()
        );
    }
    let digest = blake3::hash(&bytes).to_hex().to_string();
    if digest != artifact.blake3.to_lowercase() {
        bail!(
            "world bundle digest verification failed for {}: manifest {}, downloaded {digest}",
            artifact.url,
            artifact.blake3
        );
    }

    progress(InstallProgress::Installing);
    let root = world_root(worlds, world);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create the World directory at {}", root.display()))?;
    let scratch = root.join(tree::scratch_name("current.tmp-"));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("create the staging scratch at {}", scratch.display()))?;

    let staged_result = stage_into(&scratch, &bytes, world, &version, &digest, offers, &root);
    match staged_result {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&scratch);
            Err(error)
        }
    }
}

/// Everything after the bytes are proven, with `scratch` owned by the
/// caller's cleanup.
fn stage_into(
    scratch: &Path,
    bytes: &[u8],
    world: &str,
    version: &str,
    digest: &str,
    offers: &std::collections::BTreeMap<String, semver::Version>,
    root: &Path,
) -> Result<Outcome> {
    // The same extractor the client tree uses: one root directory stripped,
    // every path proven plain and relative before it is written. Shared rather
    // than reimplemented, because two unpackers behind one contract is the
    // drift this whole path exists to avoid.
    let files = tree::extract_tree(bytes, scratch)?;
    if files.is_empty() {
        bail!("the {world} bundle carries no files");
    }

    // The declaration travels inside the payload, so it is covered by the
    // digest already checked and inherits the feed's signature for free.
    let declared = std::fs::read(scratch.join("world.json"))
        .with_context(|| format!("the {world} bundle carries no world.json at its root"))?;
    let manifest = world_interface::manifest::WorldManifest::parse(&declared)
        .map_err(|error| anyhow!("{error}"))?;
    if manifest.id != world {
        bail!(
            "the bundle published for {world} declares itself {} — a World may not answer for another",
            manifest.id
        );
    }

    // Every release is held to one artwork contract: a real PNG, square,
    // within the size a client draws. Checked here because this is where the
    // published bytes first become an install candidate.
    for (kind, declared) in [("mark", &manifest.mark), ("hero", &manifest.hero)] {
        let Some(relative) = declared else { continue };
        let bytes = std::fs::read(scratch.join(relative)).with_context(|| {
            format!("the {world} bundle declares a {kind} at {relative} and does not carry it")
        })?;
        world_interface::manifest::artwork_bounds(kind, &bytes)
            .map_err(|why| anyhow!("the {world} bundle {why}"))?;
    }

    let unmet = manifest.unmet(offers);
    if !unmet.is_empty() {
        // Refused, not failed. The publisher shipped for a host this is not,
        // which is an ordinary state and leaves whatever serves today serving.
        let why: Vec<String> = unmet.iter().map(ToString::to_string).collect();
        for reason in &why {
            tracing::info!(%world, %version, reason, "a world bundle does not run on this build");
        }
        return Ok(Outcome::Unmet {
            version: version.to_string(),
            why,
        });
    }

    // Runner declarations are executable authority, not presentation hints.
    // Prove every runner applicable to this machine exists inside the tree
    // before sealing it, and prove there is one unambiguous formation default.
    // Launch repeats the containment proof after canonicalization so a tree
    // changed on disk still cannot turn a relative declaration into an escape.
    let applicable: Vec<_> = manifest
        .runners
        .iter()
        .filter(|runner| runner.admits(std::env::consts::OS, std::env::consts::ARCH))
        .collect();
    for runner in &applicable {
        let executable = scratch.join(&runner.program);
        if !executable.is_file() {
            bail!(
                "the {world} bundle declares runner {} and does not carry a file there",
                runner.program
            );
        }
        if let Some(cwd) = &runner.cwd {
            if !scratch.join(cwd).is_dir() {
                bail!(
                    "the {world} bundle declares runner working directory {cwd} and does not carry it"
                );
            }
        }
    }

    let releases = root.join("releases");
    std::fs::create_dir_all(&releases)
        .with_context(|| format!("create immutable World releases at {}", releases.display()))?;
    let Some(worlds) = root.parent() else {
        bail!("World {world} release root has no parent");
    };
    let Some(live) = release_dir(worlds, world, version) else {
        bail!("World {world} release version {version:?} is not semantic versioning");
    };
    // A release directory is sealed once and never replaced. A running
    // generation may still have executable and assets open from it after a
    // newer release becomes current.
    if live.exists() {
        bail!("World {world} release {version} already exists without a matching immutable record");
    }
    std::fs::rename(scratch, &live)
        .with_context(|| format!("seal the immutable release at {}", live.display()))?;

    let bundle = InstalledBundle {
        world: world.to_string(),
        version: version.to_string(),
        digest: digest.to_string(),
        files: files.len(),
    };
    let release_record = release_record_path(worlds, world, version)
        .ok_or_else(|| anyhow!("World {world} release version is not semantic versioning"))?;
    write_record(&release_record, &bundle)?;
    select(worlds, &bundle)?;

    Ok(Outcome::Staged {
        version: version.to_string(),
    })
}

/// Resolve one World's channel and stage what it holds, against the real feed.
///
/// `Err` is only ever a verification or transport failure worth reporting;
/// "this release does not run here" and "this release holds nothing for this
/// World" are ordinary [`Outcome`]s.
pub fn check(world: &str, worlds: &Path, channel: feed::Channel) -> Result<Outcome> {
    check_with_progress(world, worlds, channel, |_| {})
}

/// Resolve and install one World's signed channel release with progress.
pub fn check_with_progress<P>(
    world: &str,
    worlds: &Path,
    channel: feed::Channel,
    progress: P,
) -> Result<Outcome>
where
    P: Fn(InstallProgress) + Clone,
{
    progress(InstallProgress::Resolving);
    let pubkeys = feed::pinned_pubkeys().map_err(|error| anyhow!("{error}"))?;
    let url = pointer_url(feed::FEED_BASE_URL, world, channel);
    let resolved = feed::resolve_pointer_with(
        |asked| feed::http_fetch(asked, 1024 * 1024),
        &url,
        channel,
        &pubkeys,
        None,
    )
    .map_err(|error| anyhow!("{error}"))?;
    let download_progress = progress.clone();
    stage_bundle_with_progress(
        move |url, size| {
            let report = download_progress.clone();
            feed::http_fetch_with_progress(url, size, move |received, total| {
                report(InstallProgress::Downloading { received, total });
            })
        },
        &resolved,
        world,
        &super::facts::offered(),
        worlds,
        progress,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::feed::Channel;
    use std::collections::BTreeMap;

    const WORLD: &str = "world.lait.issues";

    fn active(worlds: &Path, world: &str) -> PathBuf {
        active_dir(worlds, world).expect("an active immutable release")
    }

    fn declaration(id: &str, version: &str, requires: serde_json::Value) -> Vec<u8> {
        serde_json::json!({
            "format": 1,
            "id": id,
            "mount": "issues",
            "version": version,
            "requires": requires,
            "launch": [{ "id": "app", "present": "primary",
                         "target": { "type": "web", "path": "/" } }],
        })
        .to_string()
        .into_bytes()
    }

    /// A bundle as `lait-feed world` packs one: gzip'd tar, one root
    /// directory, `world.json` at the bundle root.
    fn bundle(root: &str, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (path, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("{root}/{path}"), &contents[..])
                    .expect("a bundle entry appends");
            }
            builder
                .into_inner()
                .expect("the tar seals")
                .finish()
                .expect("the gzip seals");
        }
        bytes
    }

    fn ordinary(version: &str, requires: serde_json::Value) -> Vec<u8> {
        bundle(
            &format!("world-{WORLD}-{version}"),
            &[
                ("world.json".into(), declaration(WORLD, version, requires)),
                ("index.html".into(), b"<html>the head</html>".to_vec()),
            ],
        )
    }

    fn sealed(
        version: &str,
        archive: &[u8],
        size_claim: u64,
        digest_claim: &str,
    ) -> (std::collections::HashMap<String, Vec<u8>>, [u8; 32]) {
        let (seed, pubkey) = feed::tests::test_keypair();
        let url = format!("https://feed.example/releases/worlds/{WORLD}/{version}/bundle.tar.gz");
        let manifest = serde_json::json!({
            "version": version,
            "bundles": { WORLD: version },
            "artifacts": { WORLD: { ARTIFACT: {
                "url": url, "blake3": digest_claim, "size": size_claim,
            }}},
        });
        let pointer = serde_json::json!({
            "kind": "release",
            "version": version,
            "manifest": format!("https://feed.example/releases/worlds/{WORLD}/{version}/m.json"),
        });
        let mut objects = std::collections::HashMap::new();
        objects.insert(
            pointer_url("https://feed.example", WORLD, Channel::Test),
            feed::tests::seal(&pointer, &seed).into_bytes(),
        );
        objects.insert(
            format!("https://feed.example/releases/worlds/{WORLD}/{version}/m.json"),
            feed::tests::seal(&manifest, &seed).into_bytes(),
        );
        objects.insert(url, archive.to_vec());
        (objects, pubkey)
    }

    fn resolve(
        objects: &std::collections::HashMap<String, Vec<u8>>,
        pubkey: [u8; 32],
    ) -> feed::Resolved {
        feed::resolve_pointer_with(
            |url| {
                objects
                    .get(url)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &pointer_url("https://feed.example", WORLD, Channel::Test),
            Channel::Test,
            &[pubkey],
            None,
        )
        .expect("the World's signed channel resolves")
    }

    fn fetcher(
        objects: &std::collections::HashMap<String, Vec<u8>>,
    ) -> impl Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure> + '_ {
        move |url, _| {
            objects
                .get(url)
                .cloned()
                .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
        }
    }

    fn offers(control: u64) -> BTreeMap<String, semver::Version> {
        BTreeMap::from([(
            "lait.control".to_string(),
            semver::Version::new(control, 0, 0),
        )])
    }

    fn stage(
        objects: &std::collections::HashMap<String, Vec<u8>>,
        resolved: &feed::Resolved,
        offers: &BTreeMap<String, semver::Version>,
        worlds: &Path,
    ) -> Result<Outcome> {
        stage_bundle_with(fetcher(objects), resolved, WORLD, offers, worlds)
    }

    #[test]
    fn a_signed_bundle_that_runs_here_is_staged_under_its_own_world() {
        let archive = ordinary(
            "0.1.0",
            serde_json::json!([{ "name": "lait.control", "range": ">=13, <14" }]),
        );
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");

        let outcome =
            stage(&objects, &resolved, &offers(13), worlds.path()).expect("the bundle stages");
        assert_eq!(
            outcome,
            Outcome::Staged {
                version: "0.1.0".into()
            }
        );
        assert_eq!(
            std::fs::read(active(worlds.path(), WORLD).join("index.html")).expect("the entry"),
            b"<html>the head</html>"
        );
        let record = selected(worlds.path(), WORLD).expect("the record beside the payload");
        assert_eq!(record.version, "0.1.0");
        assert_eq!(record.world, WORLD);
        // Beside, never inside: everything under the payload directory may be
        // served, so a marker within it would be reachable at a URL.
        assert!(
            !active(worlds.path(), WORLD).join("selected.json").exists(),
            "the record was written inside the served payload"
        );
    }

    /// The correction this whole model exists for. A bundle whose requirements
    /// this build does not meet is refused *by name*, is not an error, and
    /// leaves whatever was serving in place.
    #[test]
    fn a_bundle_this_build_cannot_run_is_refused_by_name_and_changes_nothing() {
        let archive = ordinary(
            "0.2.0",
            serde_json::json!([{ "name": "lait.control", "range": ">=99" }]),
        );
        let (objects, pubkey) = sealed(
            "0.2.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");

        let Outcome::Unmet { version, why } =
            stage(&objects, &resolved, &offers(13), worlds.path())
                .expect("an unmet bundle is not an error")
        else {
            panic!("a bundle this build cannot run was staged");
        };
        assert_eq!(version, "0.2.0");
        assert_eq!(why.len(), 1);
        assert!(
            why[0].contains("lait.control") && why[0].contains("13.0.0"),
            "{why:?}"
        );
        assert!(
            active_dir(worlds.path(), WORLD).is_none(),
            "a refused bundle became the live payload"
        );
    }

    /// The whole point of naming facts rather than fingerprinting the host:
    /// a fact moving for an unrelated reason must leave a World working.
    #[test]
    fn a_bundle_survives_a_host_change_it_does_not_name() {
        let archive = ordinary(
            "0.1.0",
            serde_json::json!([{ "name": "lait.control", "range": ">=13, <14" }]),
        );
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        let mut host = offers(13);
        // A schema this World never names moves. Under the old fingerprint
        // every published bundle became unfetchable; here nothing moves.
        host.insert(
            "lait.world.other.schema".to_string(),
            semver::Version::new(9, 0, 0),
        );
        assert_eq!(
            stage(&objects, &resolved, &host, worlds.path()).expect("stages"),
            Outcome::Staged {
                version: "0.1.0".into()
            }
        );
    }

    /// A World that declares artwork it does not ship, or ships a banner where
    /// a square mark belongs, is refused under the common release bounds.
    #[test]
    fn declared_artwork_must_be_carried_and_must_meet_the_bounds() {
        fn png(side: u32, other: u32) -> Vec<u8> {
            let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
            bytes.extend_from_slice(&13u32.to_be_bytes());
            bytes.extend_from_slice(b"IHDR");
            bytes.extend_from_slice(&side.to_be_bytes());
            bytes.extend_from_slice(&other.to_be_bytes());
            bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
            bytes
        }
        let declaring = |art: serde_json::Value| {
            serde_json::json!({
                "format": 1, "id": WORLD, "version": "0.1.0",
                "mark": art,
                "launch": [],
            })
            .to_string()
            .into_bytes()
        };

        // Declared and not carried.
        let archive = bundle(
            "w-0.1.0",
            &[(
                "world.json".into(),
                declaring(serde_json::json!("art/mark.png")),
            )],
        );
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        let error = stage(&objects, &resolved, &offers(13), worlds.path())
            .expect_err("undelivered artwork must refuse")
            .to_string();
        assert!(error.contains("does not carry it"), "{error}");

        // Carried, and the wrong shape.
        let archive = bundle(
            "w-0.1.0",
            &[
                (
                    "world.json".into(),
                    declaring(serde_json::json!("art/mark.png")),
                ),
                ("art/mark.png".into(), png(300, 100)),
            ],
        );
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        let error = stage(&objects, &resolved, &offers(13), worlds.path())
            .expect_err("a banner where a mark belongs must refuse")
            .to_string();
        assert!(error.contains("square"), "{error}");

        // Carried and square: staged.
        let archive = bundle(
            "w-0.1.0",
            &[
                (
                    "world.json".into(),
                    declaring(serde_json::json!("art/mark.png")),
                ),
                ("art/mark.png".into(), png(196, 196)),
            ],
        );
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        assert_eq!(
            stage(&objects, &resolved, &offers(13), worlds.path()).expect("stages"),
            Outcome::Staged {
                version: "0.1.0".into()
            }
        );
    }

    #[test]
    fn a_bundle_that_answers_for_another_world_is_refused() {
        let archive = bundle(
            "world-x-0.1.0",
            &[
                (
                    "world.json".into(),
                    declaration("world.someone.else", "0.1.0", serde_json::json!([])),
                ),
                ("index.html".into(), b"x".to_vec()),
            ],
        );
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        let error = stage(&objects, &resolved, &offers(13), worlds.path())
            .expect_err("a bundle answering for another World must refuse")
            .to_string();
        assert!(error.contains("may not answer for another"), "{error}");
    }

    #[test]
    fn a_bundle_with_no_declaration_is_refused() {
        let archive = bundle("world-x-0.1.0", &[("index.html".into(), b"x".to_vec())]);
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        let error = stage(&objects, &resolved, &offers(13), worlds.path())
            .expect_err("an undeclared bundle must refuse")
            .to_string();
        assert!(error.contains("world.json"), "{error}");
    }

    #[test]
    fn a_bundle_already_staged_at_this_version_is_not_fetched_again() {
        let archive = ordinary("0.1.0", serde_json::json!([]));
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        stage(&objects, &resolved, &offers(13), worlds.path()).expect("the first stage");
        let outcome = stage_bundle_with(
            |_, _| panic!("a bundle already at this version must not be downloaded again"),
            &resolved,
            WORLD,
            &offers(13),
            worlds.path(),
        )
        .expect("the second check");
        assert_eq!(
            outcome,
            Outcome::Current {
                version: "0.1.0".into()
            }
        );
    }

    #[test]
    fn old_releases_are_never_replaced_and_can_be_selected_without_a_download() {
        let worlds = tempfile::tempdir().expect("a worlds root");
        let first = ordinary("0.1.0", serde_json::json!([]));
        let (first_objects, first_key) = sealed(
            "0.1.0",
            &first,
            first.len() as u64,
            &blake3::hash(&first).to_hex().to_string(),
        );
        let first_resolved = resolve(&first_objects, first_key);
        stage(&first_objects, &first_resolved, &offers(13), worlds.path())
            .expect("the first release stages");
        let first_dir = release_dir(worlds.path(), WORLD, "0.1.0").expect("first release path");
        let first_bytes = std::fs::read(first_dir.join("index.html")).expect("first release");

        let second = ordinary("0.2.0", serde_json::json!([]));
        let (second_objects, second_key) = sealed(
            "0.2.0",
            &second,
            second.len() as u64,
            &blake3::hash(&second).to_hex().to_string(),
        );
        let second_resolved = resolve(&second_objects, second_key);
        stage(
            &second_objects,
            &second_resolved,
            &offers(13),
            worlds.path(),
        )
        .expect("the second release stages");
        assert_eq!(
            std::fs::read(first_dir.join("index.html")).expect("first release remains"),
            first_bytes,
            "selecting a newer release changed an old immutable release"
        );

        let outcome = stage_bundle_with(
            |_, _| panic!("a retained immutable release must not be downloaded again"),
            &first_resolved,
            WORLD,
            &offers(13),
            worlds.path(),
        )
        .expect("the retained release is selected");
        assert_eq!(
            outcome,
            Outcome::Staged {
                version: "0.1.0".into()
            }
        );
        assert_eq!(
            active_dir(worlds.path(), WORLD),
            Some(first_dir),
            "future launches did not move back to the selected immutable release"
        );
    }

    #[test]
    fn republishing_an_old_version_never_overwrites_its_release() {
        let worlds = tempfile::tempdir().expect("a worlds root");
        let first = ordinary("0.1.0", serde_json::json!([]));
        let (first_objects, first_key) = sealed(
            "0.1.0",
            &first,
            first.len() as u64,
            &blake3::hash(&first).to_hex().to_string(),
        );
        let first_resolved = resolve(&first_objects, first_key);
        stage(&first_objects, &first_resolved, &offers(13), worlds.path())
            .expect("the first release stages");
        let first_dir = release_dir(worlds.path(), WORLD, "0.1.0").expect("first release path");
        let before = std::fs::read(first_dir.join("index.html")).expect("first release");

        let second = ordinary("0.2.0", serde_json::json!([]));
        let (second_objects, second_key) = sealed(
            "0.2.0",
            &second,
            second.len() as u64,
            &blake3::hash(&second).to_hex().to_string(),
        );
        let second_resolved = resolve(&second_objects, second_key);
        stage(
            &second_objects,
            &second_resolved,
            &offers(13),
            worlds.path(),
        )
        .expect("the second release stages");

        let replacement = bundle(
            "replacement-0.1.0",
            &[
                (
                    "world.json".into(),
                    declaration(WORLD, "0.1.0", serde_json::json!([])),
                ),
                ("index.html".into(), b"replaced".to_vec()),
            ],
        );
        let (replacement_objects, replacement_key) = sealed(
            "0.1.0",
            &replacement,
            replacement.len() as u64,
            &blake3::hash(&replacement).to_hex().to_string(),
        );
        let replacement_resolved = resolve(&replacement_objects, replacement_key);
        let error = stage(
            &replacement_objects,
            &replacement_resolved,
            &offers(13),
            worlds.path(),
        )
        .expect_err("a version cannot be republished with different bytes")
        .to_string();
        assert!(error.contains("immutable"), "{error}");
        assert_eq!(
            std::fs::read(first_dir.join("index.html")).expect("old release remains"),
            before
        );
        assert_eq!(
            selected(worlds.path(), WORLD).map(|bundle| bundle.version),
            Some("0.2.0".into()),
            "a refused republish changed the current selection"
        );
    }

    #[test]
    fn a_tampered_bundle_fails_the_digest_and_stages_nothing() {
        let honest = ordinary("0.1.0", serde_json::json!([]));
        let swapped = ordinary("0.1.0-evil", serde_json::json!([]));
        let (mut objects, pubkey) = sealed(
            "0.1.0",
            &honest,
            swapped.len() as u64,
            &blake3::hash(&honest).to_hex().to_string(),
        );
        let artifact = objects
            .keys()
            .find(|k| k.ends_with("bundle.tar.gz"))
            .expect("the artifact url")
            .clone();
        objects.insert(artifact, swapped);
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        let error = stage(&objects, &resolved, &offers(13), worlds.path())
            .expect_err("a tampered bundle must refuse")
            .to_string();
        assert!(error.contains("digest verification failed"), "{error}");
        assert!(
            active_dir(worlds.path(), WORLD).is_none(),
            "a refused bundle left a live payload behind"
        );
    }

    #[test]
    fn an_over_long_bundle_is_refused_by_size_before_the_digest_is_reached() {
        let archive = ordinary("0.1.0", serde_json::json!([]));
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        let worlds = tempfile::tempdir().expect("a worlds root");
        let error = stage_bundle_with(
            |url, _| {
                objects
                    .get(url)
                    .cloned()
                    .map(|mut bytes| {
                        if url.ends_with(".tar.gz") {
                            bytes.extend_from_slice(b"padding the host added");
                        }
                        bytes
                    })
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &resolved,
            WORLD,
            &offers(13),
            worlds.path(),
        )
        .expect_err("an over-long bundle must refuse")
        .to_string();
        assert!(error.contains("size mismatch"), "{error}");
    }

    /// The seam that matters: the first-party runner's independent declaration
    /// is what this host accepts when it is fetched back.
    #[test]
    fn a_world_this_build_declares_is_a_world_this_build_accepts() {
        let world = crate::world::ISSUES_ID;
        let executable = if cfg!(windows) {
            "lait-world-issues.exe"
        } else {
            "lait-world-issues"
        };
        let declaration = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/products/issues-runner/world.json.template"
        ))
        .replace("${VERSION}", "0.8.0")
        .replace("${EXE}", if cfg!(windows) { ".exe" } else { "" });

        let mut files: Vec<(String, Vec<u8>)> =
            vec![("world.json".to_string(), declaration.into_bytes())];
        files.push(("index.html".to_string(), b"<html>the head</html>".to_vec()));
        files.push((format!("bin/{executable}"), b"runner fixture".to_vec()));
        files.push((
            "art/mark.png".to_string(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/products/issues-app/assets/mark.png"
            ))
            .to_vec(),
        ));
        files.push((
            "art/hero.png".to_string(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/products/issues-app/assets/hero.png"
            ))
            .to_vec(),
        ));
        let borrowed: Vec<(&str, Vec<u8>)> =
            files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
        let archive = bundle(&format!("world-{world}-0.8.0"), &borrowed);

        let (seed, pubkey) = feed::tests::test_keypair();
        let url = format!("https://feed.example/releases/worlds/{world}/0.8.0/b.tar.gz");
        let manifest = serde_json::json!({
            "version": "0.8.0",
            "bundles": { world: "0.8.0" },
            "artifacts": { world: { ARTIFACT: {
                "url": url,
                "blake3": blake3::hash(&archive).to_hex().to_string(),
                "size": archive.len(),
            }}},
        });
        let pointer = serde_json::json!({
            "kind": "release", "version": "0.8.0",
            "manifest": format!("https://feed.example/releases/worlds/{world}/0.8.0/m.json"),
        });
        let mut objects = std::collections::HashMap::new();
        objects.insert(
            pointer_url("https://feed.example", world, Channel::Test),
            feed::tests::seal(&pointer, &seed).into_bytes(),
        );
        objects.insert(
            format!("https://feed.example/releases/worlds/{world}/0.8.0/m.json"),
            feed::tests::seal(&manifest, &seed).into_bytes(),
        );
        objects.insert(url, archive);

        let resolved = feed::resolve_pointer_with(
            |asked| {
                objects
                    .get(asked)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {asked}")))
            },
            &pointer_url("https://feed.example", world, Channel::Test),
            Channel::Test,
            &[pubkey],
            None,
        )
        .expect("the channel resolves");

        let worlds = tempfile::tempdir().expect("a worlds root");
        // Against the facts this build really offers, not a fixture: if the
        // declaration asked for something we do not provide, this is where a
        // release nobody could install would be caught.
        let outcome = stage_bundle_with(
            fetcher(&objects),
            &resolved,
            world,
            &crate::update::facts::offered(),
            worlds.path(),
        )
        .expect("the World this build declares stages here");
        assert_eq!(
            outcome,
            Outcome::Staged {
                version: "0.8.0".into()
            }
        );
        assert_eq!(
            std::fs::read(active(worlds.path(), world).join("index.html")).expect("the head"),
            b"<html>the head</html>"
        );
    }

    /// A World's pointer is its own object, one level inside the product's
    /// channel layout, and resolved by the same function — so it inherits
    /// every rule rather than getting a weaker copy.
    #[test]
    fn a_world_channel_sits_beside_the_products_and_under_the_same_rules() {
        assert_eq!(
            pointer_url("https://feed.example/", WORLD, Channel::Test),
            "https://feed.example/channels/worlds/world.lait.issues/test"
        );
        let (seed, pubkey) = feed::tests::test_keypair();
        let pointer = serde_json::json!({
            "kind": "release", "version": "0.2.0-test.1",
            "manifest": "https://feed.example/m.json",
        });
        let objects = std::collections::HashMap::from([(
            pointer_url("https://feed.example", WORLD, Channel::Stable),
            feed::tests::seal(&pointer, &seed).into_bytes(),
        )]);
        let failure = feed::resolve_pointer_with(
            |url| {
                objects
                    .get(url)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &pointer_url("https://feed.example", WORLD, Channel::Stable),
            Channel::Stable,
            &[pubkey],
            None,
        )
        .expect_err("a prerelease on a World's stable channel must refuse");
        assert!(matches!(failure, feed::Failure::Invalid(_)), "{failure:?}");
    }

    /// Two Worlds must not share a directory. The first cut keyed staging by
    /// runtime alone, so the second World overwrote the first and both
    /// re-downloaded every period.
    #[test]
    fn two_worlds_stage_side_by_side_without_touching_each_other() {
        let worlds = tempfile::tempdir().expect("a worlds root");
        let archive = ordinary("0.1.0", serde_json::json!([]));
        let (objects, pubkey) = sealed(
            "0.1.0",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey);
        stage(&objects, &resolved, &offers(13), worlds.path()).expect("the first World stages");

        // A second World, published under its own id, staged from its own
        // bundle into its own directory.
        let other = "world.lait.signage";
        let other_archive = bundle(
            &format!("world-{other}-0.1.0"),
            &[
                (
                    "world.json".into(),
                    declaration(other, "0.1.0", serde_json::json!([])),
                ),
                ("index.html".into(), b"the other head".to_vec()),
            ],
        );
        let url = format!("https://feed.example/releases/worlds/{other}/0.1.0/bundle.tar.gz");
        let (seed, pubkey2) = feed::tests::test_keypair();
        let manifest = serde_json::json!({
            "version": "0.1.0",
            "bundles": { other: "0.1.0" },
            "artifacts": { other: { ARTIFACT: {
                "url": url,
                "blake3": blake3::hash(&other_archive).to_hex().to_string(),
                "size": other_archive.len(),
            }}},
        });
        let pointer = serde_json::json!({
            "kind": "release", "version": "0.1.0",
            "manifest": format!("https://feed.example/releases/worlds/{other}/0.1.0/m.json"),
        });
        let mut objects2 = std::collections::HashMap::new();
        objects2.insert(
            pointer_url("https://feed.example", other, Channel::Test),
            feed::tests::seal(&pointer, &seed).into_bytes(),
        );
        objects2.insert(
            format!("https://feed.example/releases/worlds/{other}/0.1.0/m.json"),
            feed::tests::seal(&manifest, &seed).into_bytes(),
        );
        objects2.insert(url, other_archive);
        let resolved2 = feed::resolve_pointer_with(
            |url| {
                objects2
                    .get(url)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &pointer_url("https://feed.example", other, Channel::Test),
            Channel::Test,
            &[pubkey2],
            None,
        )
        .expect("the second World's channel resolves");
        stage_bundle_with(
            fetcher(&objects2),
            &resolved2,
            other,
            &offers(13),
            worlds.path(),
        )
        .expect("the second World stages");

        assert_eq!(
            std::fs::read(active(worlds.path(), WORLD).join("index.html")).expect("the first"),
            b"<html>the head</html>",
            "the second World overwrote the first"
        );
        assert_eq!(
            std::fs::read(active(worlds.path(), other).join("index.html")).expect("the second"),
            b"the other head"
        );
    }

    /// The whole point of the recorded standing is that a surface can draw an
    /// update control from it — so every way of *not knowing* must answer
    /// "not behind". A control offered on a guess is worse than none, because
    /// pressing it produces nothing and teaches the person to distrust it.
    #[test]
    fn only_a_known_newer_bundle_this_build_can_run_is_behind() {
        let known = |channel: Option<&str>, serving: Option<&str>, unmet: Option<Vec<String>>| {
            Standing {
                serving: serving.map(str::to_string),
                channel: channel.map(str::to_string),
                checked_at: 1,
                unmet,
            }
            .behind()
        };

        assert!(
            known(Some("0.9.1"), Some("0.9.0"), None),
            "a channel ahead of what is serving is the one case that is behind"
        );
        assert!(
            known(Some("0.9.0"), None, None),
            "a published bundle with no selected release serving is behind"
        );
        assert!(
            !known(Some("0.9.0"), Some("0.9.0"), None),
            "the channel and the serving bundle agree; nothing to offer"
        );
        assert!(
            !known(None, Some("0.9.0"), None),
            "a channel that named nothing for us is not a channel that is ahead"
        );
        assert!(
            !known(None, None, None),
            "a machine that has never learned anything is not behind"
        );
        assert!(
            !known(
                Some("1.0.0"),
                Some("0.9.0"),
                Some(vec!["control >=3".into()])
            ),
            "a bundle this build cannot run is not an update a person can take"
        );
    }

    /// A refusal must not erase what is still serving. The invariant is that
    /// the previous bundle keeps serving when a newer one is unmet, and the
    /// standing is what a surface reads to say so.
    #[test]
    fn an_unmet_bundle_records_the_refusal_and_keeps_naming_what_serves() {
        let worlds = tempfile::tempdir().expect("a worlds root");
        let live = release_dir(worlds.path(), WORLD, "0.9.0").expect("release path");
        std::fs::create_dir_all(&live).expect("a live tree");
        std::fs::write(
            record_path(worlds.path(), WORLD),
            serde_json::to_vec(&InstalledBundle {
                world: WORLD.to_string(),
                version: "0.9.0".into(),
                digest: "00".repeat(32),
                files: 1,
            })
            .expect("a record"),
        )
        .expect("write the record");

        note(
            worlds.path(),
            WORLD,
            &Outcome::Unmet {
                version: "1.0.0".into(),
                why: vec!["control >=3, <4".into()],
            },
            1_700_000_000,
        );

        let standing = standing(worlds.path(), WORLD).expect("a standing was recorded");
        assert_eq!(standing.serving.as_deref(), Some("0.9.0"));
        assert_eq!(standing.channel.as_deref(), Some("1.0.0"));
        assert_eq!(
            standing.unmet.as_deref(),
            Some(&["control >=3, <4".to_string()][..])
        );
        assert!(
            !standing.behind(),
            "an unmet bundle must not render as an update waiting to be taken"
        );
    }

    /// `note` is written beside the served tree, never inside it — a marker
    /// under `current/` is reachable at a URL, and a served tree holds only
    /// what its publisher put there.
    #[test]
    fn the_standing_is_recorded_beside_the_served_tree_and_never_within_it() {
        let worlds = tempfile::tempdir().expect("a worlds root");
        note(
            worlds.path(),
            WORLD,
            &Outcome::Current {
                version: "0.9.0".into(),
            },
            1_700_000_000,
        );
        assert!(
            standing_path(worlds.path(), WORLD).is_file(),
            "no standing was written"
        );
        assert!(
            !release_dir(worlds.path(), WORLD, "0.9.0")
                .expect("release path")
                .join("standing.json")
                .is_file(),
            "the standing landed inside the served tree, where it is reachable at a URL"
        );
    }
}
