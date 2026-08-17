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
    /// meet. Whatever was already in place keeps serving — which may be the
    /// embedded floor — and each unmet requirement is named.
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

/// What sits under `worlds/<world>/current/`, recorded beside it rather than
/// inside.
///
/// Beside, because everything inside that directory may be *served*: a marker
/// within it would be reachable at a URL, and a served tree must hold only
/// what its publisher put there.
#[derive(Debug, Serialize, Deserialize)]
pub struct StagedBundle {
    /// The World this bundle belongs to.
    pub world: String,
    /// The bundle version.
    pub version: String,
    /// How many files it holds.
    pub files: usize,
}

/// The directory a World's payloads live under, one per World.
pub fn world_root(worlds: &Path, world: &str) -> PathBuf {
    worlds.join(world)
}

/// The live payload directory for a World.
pub fn live_dir(worlds: &Path, world: &str) -> PathBuf {
    world_root(worlds, world).join("current")
}

fn record_path(worlds: &Path, world: &str) -> PathBuf {
    world_root(worlds, world).join("current.json")
}

/// What is staged for a World, when anything is.
pub fn staged(worlds: &Path, world: &str) -> Option<StagedBundle> {
    if !live_dir(worlds, world).is_dir() {
        return None;
    }
    let bytes = std::fs::read(record_path(worlds, world)).ok()?;
    serde_json::from_slice(&bytes).ok()
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
/// One artifact per release, because what a bundle can run against is stated
/// in its own declaration rather than encoded in where it is filed. A payload
/// that genuinely differs per platform states that with a `when` on its launch
/// entries and ships both, which is how every launcher surveyed does it.
pub const ARTIFACT: &str = "any";

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

    if staged(worlds, world).is_some_and(|bundle| bundle.version == version) {
        return Ok(Outcome::Current { version });
    }

    let bytes = fetch(&artifact.url, artifact.size)
        .map_err(|error| anyhow!("world bundle download: {error}"))?;
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

    let root = world_root(worlds, world);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create the World directory at {}", root.display()))?;
    let scratch = root.join(tree::scratch_name("current.tmp-"));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("create the staging scratch at {}", scratch.display()))?;

    let staged_result = stage_into(&scratch, &bytes, world, &version, offers, &root);
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

    let live = root.join("current");
    let record = root.join("current.json");
    // The record goes first, so from here until it is written again there is
    // no believable staged bundle — the state every reader must see while the
    // directory beneath it is being replaced.
    let _ = std::fs::remove_file(&record);
    if live.exists() {
        // Rename aside rather than delete in place: a rename onto an existing
        // directory is an error on Windows, and a delete there routinely
        // loses to a scanner holding a file open.
        let aside = root.join(tree::scratch_name("current.old-"));
        std::fs::rename(&live, &aside)
            .with_context(|| format!("set aside the prior bundle at {}", live.display()))?;
        let _ = std::fs::remove_dir_all(&aside);
    }
    std::fs::rename(scratch, &live)
        .with_context(|| format!("move the staged bundle into place at {}", live.display()))?;

    let bundle = StagedBundle {
        world: world.to_string(),
        version: version.to_string(),
        files: files.len(),
    };
    let encoded = serde_json::to_vec_pretty(&bundle).context("encode the staged bundle record")?;
    let staging = root.join(tree::scratch_name("current.json.tmp-"));
    std::fs::write(&staging, encoded).context("write the staged bundle record")?;
    std::fs::rename(&staging, &record).context("seal the staged bundle record")?;

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
    stage_bundle_with(
        feed::http_fetch,
        &resolved,
        world,
        &super::facts::offered(),
        worlds,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::feed::Channel;
    use std::collections::BTreeMap;

    const WORLD: &str = "world.lait.issues";

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
            std::fs::read(live_dir(worlds.path(), WORLD).join("index.html")).expect("the entry"),
            b"<html>the head</html>"
        );
        let record = staged(worlds.path(), WORLD).expect("the record beside the payload");
        assert_eq!(record.version, "0.1.0");
        assert_eq!(record.world, WORLD);
        // Beside, never inside: everything under the payload directory may be
        // served, so a marker within it would be reachable at a URL.
        assert!(
            !live_dir(worlds.path(), WORLD).join("current.json").exists(),
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
            !live_dir(worlds.path(), WORLD).exists(),
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
            !live_dir(worlds.path(), WORLD).exists(),
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
            std::fs::read(live_dir(worlds.path(), WORLD).join("index.html")).expect("the first"),
            b"<html>the head</html>",
            "the second World overwrote the first"
        );
        assert_eq!(
            std::fs::read(live_dir(worlds.path(), other).join("index.html")).expect("the second"),
            b"the other head"
        );
    }
}
